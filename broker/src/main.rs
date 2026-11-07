// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
// (In a test build, the test harness itself marks `main` `allow(dead_code)`, which `forbid` would
// reject: there it is `deny`. The real binary, which the Nix build compiles, forbids it, so an
// `allow(dead_code)` in the source never builds.)
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

use anyhow::{Context, Result};

mod serve;

use torq_broker::config::{self, Requested, Settings};
use torq_broker::{ledger::Entry, policy::Actor, Broker, Busy};
use serde_json::json;
use std::path::{Path, PathBuf};

/// TorqOS change broker — the only writer of the system config repo.
///
/// Run as root on a TorqOS machine, it takes its settings only from /etc/torq/broker.json and
/// refuses flags or TORQ_* variables that differ from it. Everything it runs gets a fixed, cleared
/// environment.
struct Cli {
    repo: Option<PathBuf>,
    state_dir: Option<PathBuf>,
    rebuild_cmd: Option<String>,
    build_cmd: Option<String>,
    activate_cmd: Option<String>,
    boot_cmd: Option<String>,
    schedule_cmd: Option<String>,
    current_system: Option<PathBuf>,
    system_profile: Option<PathBuf>,
    json: bool,
    cmd: Cmd,
}

enum Cmd {
    Init,
    Setup,
    Password { user: String, hash_file: PathBuf, approved_by: String },
    Propose { diff: PathBuf, reason: String, actor: Actor, tainted: bool, base: Option<String> },
    Show { seq: u64 },
    Apply { seq: u64, approved_by: String, confirm_tainted: bool, confirm: Option<u32> },
    Confirm { approved_by: String },
    ExpireConfirm { seq: Option<u64>, reboot: bool },
    Revert { seq: u64, approved_by: String },
    Reconcile { approved_by: String },
    Repair { approved_by: String },
    Log { brief: bool },
    Verify,
    Status,
    Serve,
    RemoteKey,
    Push,
    RemoteShow,
    RemoteClear { approved_by: String },
    Acknowledge { approved_by: String },
    FetchIncoming,
    BreakGlass { by: String, reason: String },
    Snapshots,
    VerifyStore,
    RestoreData { seq: u64, path: String, approved_by: String },
    SnapshotTake { approved_by: String },
    SnapshotDelete { seq: u64, approved_by: String },
    AdoptKey { url: String },
    AdoptInspect { url: String },
    Adopt { url: String, own: PathBuf, head: String, tip: String, user: String },
}

/// The command line, declared with clap's builder (its derive macros write an `allow(dead_code)`,
/// which this crate forbids): every flag, its variable, its help — as `torq-broker --help` shows.
fn command_line() -> clap::Command {
    use clap::{value_parser, Arg, ArgAction, Command};
    let opt = |name: &'static str, var: &'static str, help: &'static str| Arg::new(name).long(name).env(var).help(help);
    let path = |name: &'static str, var: &'static str, help: &'static str| opt(name, var, help).value_parser(value_parser!(PathBuf));
    let flag = |name: &'static str, help: &'static str| Arg::new(name).long(name).action(ArgAction::SetTrue).help(help);
    let by = || Arg::new("approved-by").long("approved-by").required(true);
    let seq = || Arg::new("seq").required(true).value_parser(value_parser!(u64));
    Command::new("torq-broker")
        .version(env!("CARGO_PKG_VERSION"))
        .about("TorqOS change broker — the only writer of the system config repo.")
        .long_about(
            "TorqOS change broker — the only writer of the system config repo.\n\nRun as root on a TorqOS machine, it takes its \
             settings only from /etc/torq/broker.json and refuses flags or TORQ_* variables that differ from it. Everything it runs gets a \
             fixed, cleared environment.",
        )
        .subcommand_required(true)
        .arg(path("repo", "TORQ_CONFIG_REPO", "Path to the config git repository [default: /var/lib/torq/config]."))
        .arg(path("state-dir", "TORQ_STATE_DIR", "Directory for the ledger and broker state [default: /var/lib/torq/state]."))
        .arg(opt(
            "rebuild-cmd",
            "TORQ_REBUILD_CMD",
            "Command run after each commit; `{flake}` is the config repo at the commit (git+file://…?rev=…), `{repo}` its path [default: build the commit with nix, make it the system profile, switch to it in a transient unit (torq-switch)].",
        ))
        .arg(opt(
            "build-cmd",
            "TORQ_BUILD_CMD",
            "`apply --confirm`: build the system and print its store path; `{flake}`, `{repo}` [default: nix build --no-update-lock-file --out-link {link} --print-out-paths {flake}#nixosConfigurations.torq...].",
        ))
        .arg(opt("activate-cmd", "TORQ_ACTIVATE_CMD", "Activate an existing system (rollback of an unconfirmed commit); `{system}`."))
        .arg(opt("boot-cmd", "TORQ_BOOT_CMD", "Make a system the boot default (`confirm`); `{system}`, `{profile}`."))
        .arg(opt("schedule-cmd", "TORQ_SCHEDULE_CMD", "Schedule `{broker} expire-confirm {seq}` after `{seconds}` [default: a transient systemd timer]."))
        .arg(path(
            "current-system",
            "TORQ_CURRENT_SYSTEM",
            "Link to the running system, recorded after every rebuild and compared by `status` [default: /run/current-system].",
        ))
        .arg(path(
            "system-profile",
            "TORQ_SYSTEM_PROFILE",
            "The system profile: what the next boot starts, compared by `status` [default: /nix/var/nix/profiles/system].",
        ))
        .arg(flag(
            "json",
            "Machine-readable output: one JSON value per command (a ledger entry, or the status), and `{\"error\": \"...\"}` on failure. For the `torq` CLI (ADR-014); people get plain text.",
        ).global(true))
        .subcommand(Command::new("init").about("Initialise the config repo and ledger (idempotent)."))
        .subcommand(
            Command::new("password")
                .about("Set a person's password: its hash (SHA-512 or yescrypt crypt) read from a file, set at once and kept outside the config repo; ledgered without the hash.")
                .arg(Arg::new("user").long("user").required(true))
                .arg(Arg::new("hash-file").long("hash-file").required(true).value_parser(value_parser!(PathBuf)))
                .arg(by()),
        )
        .subcommand(Command::new("setup").about("First-boot setup: take over the config repo just written, build and switch to it, ledger it (once)."))
        .subcommand(
            Command::new("propose")
                .about("Submit a change (unified diff against the repo) for review.")
                .arg(Arg::new("diff").long("diff").required(true).value_parser(value_parser!(PathBuf)))
                .arg(Arg::new("reason").long("reason").required(true))
                .arg(Arg::new("actor").long("actor").default_value("user").value_parser(["user", "agent"]))
                .arg(flag("tainted", "The proposing session ingested untrusted content (web, email, downloads)."))
                .arg(Arg::new("base").long("base").help("The commit this change was prepared against: refused if the configuration has moved on.")),
        )
        .subcommand(Command::new("show").about("Show a proposal or applied change.").arg(seq()))
        .subcommand(
            Command::new("apply")
                .about("Approve and apply a proposal: commit, rebuild, ledger.")
                .arg(seq())
                .arg(by())
                .arg(Arg::new("confirm-tainted").long("confirm-tainted").action(ArgAction::SetTrue))
                .arg(Arg::new("confirm").long("confirm").value_name("MINUTES").value_parser(value_parser!(u32)).help(
                    "Commit confirmed: activate without making it the boot default, and roll it back unless `confirm` runs within this many minutes (a reboot rolls it back too).",
                )),
        )
        .subcommand(Command::new("confirm").about("Keep the commit confirmed that is waiting: make it the boot default.").arg(by()))
        .subcommand(
            Command::new("expire-confirm")
                .about("Roll back the commit confirmed that is waiting (its timer runs this at the deadline).")
                .arg(Arg::new("seq").value_parser(value_parser!(u64)).help("Only if this applied change, or the timer it names, is the one waiting."))
                .arg(flag("reboot", "Run at boot: the machine restarted before the change was confirmed.")),
        )
        .subcommand(Command::new("revert").about("Undo one applied change: a new commit that reverts it, rebuild, ledger.").arg(seq()).arg(by()))
        .subcommand(
            Command::new("reconcile")
                .about("Finish an apply or revert whose broker died: discard its unledgered commit, reset to the last ledgered config, rebuild, and ledger it. apply and revert do this first on their own.")
                .arg(by()),
        )
        .subcommand(
            Command::new("repair")
                .about("Return the machine to its last ledgered configuration after drift (switched outside the broker, a changed boot default, a failed restore): rebuild it and ledger that. Never automatic (ADR-012). Refuses when the config repo itself was changed outside the broker.")
                .arg(by()),
        )
        .subcommand(Command::new("log").about("Print the ledger.").arg(flag("brief", "Leave out proposals' full diffs (for listing: the diffs make the ledger large).")))
        .subcommand(Command::new("verify").about("Verify the ledger hash chain."))
        .subcommand(Command::new("status").about(
            "Compare the machine with its ledger: running system, config repo, interrupted operations, ledger chain. Exits 1 if anything is not as ledgered, 75 if an operation is running.",
        ))
        .subcommand(Command::new("remote-key").about("The machine's key for the config repo's remote (ADR-036): made the first time, kept after; its public half printed, to add to the repository as a deploy key with write access."))
        .subcommand(Command::new("push").about("Push the configuration and the ledger to the remote now (after every change it is pushed anyway; this is the timer's retry): the remote's ledger checked first."))
        .subcommand(Command::new("remote").about("The remote: its URL, the last push, what its ledger said of this one then, and its alarm, if any."))
        .subcommand(Command::new("adopt-key").about("First-boot setup: this machine's key for a repository it is about to adopt (ADR-036), made the first time.").arg(Arg::new("url").long("url").required(true)))
        .subcommand(Command::new("adopt-inspect").about("First-boot setup: fetch the repository into a staging copy and check it (its ledger one line, every push extending the last, its chain whole, main the commit it ends at); changes nothing of the machine.").arg(Arg::new("url").long("url").required(true)))
        .subcommand(Command::new("adopt").about("First-boot setup: become the machine the repository holds: its history and ledger, this machine's hardware file and software (the files in --own) as a new commit, built, switched to, ledgered.").arg(Arg::new("url").long("url").required(true)).arg(Arg::new("own").long("own").required(true).value_parser(value_parser!(PathBuf))).arg(Arg::new("head").long("head").required(true)).arg(Arg::new("tip").long("tip").required(true)).arg(Arg::new("user").long("user").required(true)))
        .subcommand(Command::new("break-glass").about("A root shell is about to open (torq break-glass, ADR-035): who and why into the ledger, and to the remote if there is one. Exits 3 if the remote has not got it.").arg(Arg::new("by").long("by").required(true)).arg(Arg::new("reason").long("reason").required(true)))
        .subcommand(Command::new("snapshot-take").about("A snapshot of the data now (ADR-038), ledgered.").arg(by()))
        .subcommand(Command::new("snapshot-delete").about("The snapshot of a change gone (ADR-038): on the record first.").arg(seq()).arg(by()))
        .subcommand(Command::new("verify-store").about("Check the running system's store paths against what the store recorded when each was built (the audit of M3, M5); kept for status."))
        .subcommand(Command::new("snapshots").about("The snapshots of the data this machine still has (ADR-038)."))
        .subcommand(Command::new("restore-data").about("Put a path back as it was before a change (ADR-038): as its owner, what is there set aside, ledgered.").arg(seq()).arg(Arg::new("path").long("path").required(true)).arg(by()))
        .subcommand(Command::new("fetch-incoming").about("Fetch the remote's `incoming` branch into the config repo (refs/torq/incoming), for torqcli's `request system pull` to load into a candidate: nothing is applied."))
        .subcommand(Command::new("ledger-acknowledge").about("A person has looked into where the ledger and the repository disagree: said no more, and on the record.").arg(by()))
        .subcommand(Command::new("remote-clear").about("Clear the remote's alarm, having looked into it (ledgered): the next push checks the remote's whole history again, and raises it again if it is still true.").arg(by()))
        .subcommand(Command::new("serve").about(
            "Answer one request on the socket (torq-broker.socket hands it the connection as standard input and output): the caller from the kernel (SO_PEERCRED), what it may do by its account and the change's tier, a system change's password through PAM (ADR-040).",
        ))
}

/// The command line read into `Cli` (clap exits with its usage on a mistake, as before).
fn parse_cli() -> Cli {
    let m = command_line().get_matches();
    let s = |m: &clap::ArgMatches, k: &str| m.get_one::<String>(k).cloned();
    let p = |m: &clap::ArgMatches, k: &str| m.get_one::<PathBuf>(k).cloned();
    let by = |m: &clap::ArgMatches| s(m, "approved-by").unwrap_or_default();
    let seq = |m: &clap::ArgMatches| m.get_one::<u64>("seq").copied();
    let (name, sub) = m.subcommand().expect("a subcommand is required");
    let cmd = match name {
        "init" => Cmd::Init,
        "setup" => Cmd::Setup,
        "password" => Cmd::Password { user: s(sub, "user").unwrap_or_default(), hash_file: p(sub, "hash-file").unwrap_or_default(), approved_by: by(sub) },
        "propose" => Cmd::Propose {
            diff: p(sub, "diff").unwrap_or_default(),
            reason: s(sub, "reason").unwrap_or_default(),
            actor: s(sub, "actor").unwrap_or_default().parse().unwrap_or(Actor::User),
            tainted: sub.get_flag("tainted"),
            base: s(sub, "base"),
        },
        "show" => Cmd::Show { seq: seq(sub).unwrap_or_default() },
        "apply" => Cmd::Apply {
            seq: seq(sub).unwrap_or_default(),
            approved_by: by(sub),
            confirm_tainted: sub.get_flag("confirm-tainted"),
            confirm: sub.get_one::<u32>("confirm").copied(),
        },
        "confirm" => Cmd::Confirm { approved_by: by(sub) },
        "expire-confirm" => Cmd::ExpireConfirm { seq: seq(sub), reboot: sub.get_flag("reboot") },
        "revert" => Cmd::Revert { seq: seq(sub).unwrap_or_default(), approved_by: by(sub) },
        "reconcile" => Cmd::Reconcile { approved_by: by(sub) },
        "repair" => Cmd::Repair { approved_by: by(sub) },
        "log" => Cmd::Log { brief: sub.get_flag("brief") },
        "verify" => Cmd::Verify,
        "status" => Cmd::Status,
        "serve" => Cmd::Serve,
        "remote-key" => Cmd::RemoteKey,
        "push" => Cmd::Push,
        "remote" => Cmd::RemoteShow,
        "remote-clear" => Cmd::RemoteClear { approved_by: by(sub) },
        "ledger-acknowledge" => Cmd::Acknowledge { approved_by: by(sub) },
        "fetch-incoming" => Cmd::FetchIncoming,
        "snapshots" => Cmd::Snapshots,
        "verify-store" => Cmd::VerifyStore,
        "snapshot-take" => Cmd::SnapshotTake { approved_by: by(sub) },
        "snapshot-delete" => Cmd::SnapshotDelete { seq: *sub.get_one::<u64>("seq").unwrap_or(&0), approved_by: by(sub) },
        "restore-data" => Cmd::RestoreData { seq: *sub.get_one::<u64>("seq").unwrap_or(&0), path: s(sub, "path").unwrap_or_default(), approved_by: by(sub) },
        "break-glass" => Cmd::BreakGlass { by: s(sub, "by").unwrap_or_default(), reason: s(sub, "reason").unwrap_or_default() },
        "adopt-key" => Cmd::AdoptKey { url: s(sub, "url").unwrap_or_default() },
        "adopt-inspect" => Cmd::AdoptInspect { url: s(sub, "url").unwrap_or_default() },
        "adopt" => Cmd::Adopt { url: s(sub, "url").unwrap_or_default(), own: p(sub, "own").unwrap_or_default(), head: s(sub, "head").unwrap_or_default(), tip: s(sub, "tip").unwrap_or_default(), user: s(sub, "user").unwrap_or_default() },
        other => unreachable!("clap knows no subcommand {other}"),
    };
    Cli {
        repo: p(&m, "repo"),
        state_dir: p(&m, "state-dir"),
        rebuild_cmd: s(&m, "rebuild-cmd"),
        build_cmd: s(&m, "build-cmd"),
        activate_cmd: s(&m, "activate-cmd"),
        boot_cmd: s(&m, "boot-cmd"),
        schedule_cmd: s(&m, "schedule-cmd"),
        current_system: p(&m, "current-system"),
        system_profile: p(&m, "system-profile"),
        json: m.get_flag("json"),
        cmd,
    }
}

fn main() {
    let cli = parse_cli();
    if matches!(cli.cmd, Cmd::Serve) {
        std::process::exit(serve::serve());
    }
    let json = cli.json;
    // How the request came, read before the environment is cleared: from `serve` (the socket), or
    // through sudo — what root says about itself, marked as such (ADR-040). Neither: the machine's
    // own units, setup, or root at a shell.
    let (via, caller) = match (std::env::var("TORQ_VIA").ok().as_deref(), std::env::var_os("SUDO_UID")) {
        (Some("socket"), _) if config::running_as_root() => (Some("socket".to_string()), std::env::var("TORQ_CALLER").ok()),
        (_, Some(_)) => (Some("sudo".to_string()), None),
        _ => (None, None),
    };
    if let Err(e) = run(cli, via, caller) {
        // Another operation is running: "try again later", exit 75 (EX_TEMPFAIL) from any command.
        let busy = e.downcast_ref::<Busy>().is_some();
        if json {
            println!("{}", json!({ "error": format!("{e:#}"), "busy": busy }));
        } else {
            eprintln!("Error: {e:?}");
        }
        std::process::exit(if busy { 75 } else { 1 });
    }
}

fn print_entry(e: &Entry) -> Result<()> {
    println!("{}", serde_json::to_string(e)?);
    Ok(())
}

/// Resolve the settings (config.rs), then take over from the caller: every child (git, sh, the
/// rebuild) gets the broker's own environment and `/` (`config::command`), and files its umask, and
/// nothing of the caller's — GIT_*, NIX_*, LD_*, their HOME and its .gitconfig, their
/// current directory — may reach a root broker's work. Path arguments are made absolute first, so a
/// relative `--diff` still means what the caller meant.
fn settings(cli: &mut Cli) -> Result<Settings> {
    let abs = |p: &mut PathBuf| -> Result<()> {
        *p = std::path::absolute(&*p).with_context(|| format!("resolving {}", p.display()))?;
        Ok(())
    };
    for p in [&mut cli.repo, &mut cli.state_dir, &mut cli.current_system, &mut cli.system_profile].into_iter().flatten() {
        abs(p)?;
    }
    if let Cmd::Propose { diff, .. } = &mut cli.cmd {
        abs(diff)?;
    }
    let root = config::running_as_root();
    let path = std::env::var("PATH").unwrap_or_default();
    let home = if root { PathBuf::from("/root") } else { std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()) };
    let asked = Requested {
        repo: cli.repo.clone(),
        state_dir: cli.state_dir.clone(),
        rebuild: cli.rebuild_cmd.clone(),
        build: cli.build_cmd.clone(),
        activate: cli.activate_cmd.clone(),
        boot: cli.boot_cmd.clone(),
        schedule: cli.schedule_cmd.clone(),
        current_system: cli.current_system.clone(),
        system_profile: cli.system_profile.clone(),
    };
    // Only a root broker uses the file, so only it reads it: a malformed or newer one must not
    // break a test broker. The rollback of an unconfirmed commit is run by the broker that made it,
    // which may be older than the file the change installed: it reads leniently.
    let lenient = matches!(cli.cmd, Cmd::ExpireConfirm { .. });
    let fixed = if root { Settings::read(Path::new(config::FIXED), lenient)? } else { None };
    let s = config::resolve(root, fixed, asked, Settings::defaults(path, home))?;
    let inherited = std::env::vars_os().filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    config::take_over_process(&config::environment(&s, inherited))?;
    Ok(s)
}

fn run(mut cli: Cli, via: Option<String>, caller: Option<String>) -> Result<()> {
    let json = cli.json;
    let s = settings(&mut cli)?;
    let mut b = Broker::new(&s.repo, &s.state_dir, &s.rebuild)
        .with_current_system(&s.current_system)
        .with_system_profile(&s.system_profile)
        .with_confirm_commands(&s.build, &s.activate, &s.boot, &s.schedule)
        .with_snapshots(&s.snapshot)
        .with_people(&s.people)
        .with_agent_limits(&s.agent)
        .with_remote(s.remote.as_deref())
        // On a real machine the rollback uses the old system's own commands (its etc/torq/broker.json).
        .with_anchor_settings(config::running_as_root());
    b.ledger.set_via(via, caller);
    // Whatever a command that writes the ledger does — and however it ends — the remote gets it
    // afterwards (ADR-036): best-effort, never failing the command.
    let writes = matches!(
        cli.cmd,
        Cmd::Init | Cmd::Setup | Cmd::Password { .. } | Cmd::Propose { .. } | Cmd::Apply { .. } | Cmd::Confirm { .. }
            | Cmd::ExpireConfirm { .. } | Cmd::Revert { .. } | Cmd::Reconcile { .. } | Cmd::Repair { .. } | Cmd::RemoteClear { .. } | Cmd::RestoreData { .. } | Cmd::Acknowledge { .. } | Cmd::SnapshotTake { .. } | Cmd::SnapshotDelete { .. }
    );
    let _push = writes.then_some(PushAfter(&b, b.ledger.read_all().map_or(0, |l| l.len())));
    match cli.cmd {
        Cmd::Serve => unreachable!("served before the settings are read"),
        Cmd::RemoteKey => {
            let r = b.remote()?.context("no remote is set: `torq remote url <ssh url>` first (a system change)")?;
            let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
            let key = r.public_key(host.trim())?;
            if json {
                println!("{}", json!({ "public_key": key, "url": r.url }));
            } else {
                println!("{key}\n\nAdd it to {} as a deploy key, with write access; then: torq-broker push", r.url);
            }
        }
        Cmd::Push => match (b.sync_remote()?, json) {
            (p, true) => println!("{}", json!({ "pushed": p })),
            (None, false) => println!("no remote is set (or an operation is running, which pushes when it is done)"),
            (Some(p), false) if p.ok => println!("pushed: the remote has the configuration and {} ledger entries", p.entries),
            (Some(p), false) => {
                println!("not pushed: {}", printable(p.error.as_deref().unwrap_or("?")));
                std::process::exit(1);
            }
        },
        Cmd::AdoptKey { url } => {
            let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
            let key = b.adopt_key(&url, host.trim())?;
            println!("{}", if json { json!({ "public_key": key }).to_string() } else { key });
        }
        Cmd::AdoptInspect { url } => {
            let a = b.adopt_inspect(&url)?;
            println!("{}", if json { serde_json::to_string(&a)? } else { format!("{a:?}") });
        }
        Cmd::Adopt { url, own, head, tip, user } => {
            let e = b.adopt(&url, &own, &head, &tip, &user)?;
            if json {
                return print_entry(&e);
            }
            println!("{} (ledger #{})", printable(&e.body.reason), e.body.seq);
        }
        Cmd::BreakGlass { by, reason } => {
            let (e, unsent) = b.break_glass(&by, &reason)?;
            println!("on the record: ledger #{} ({} breaks the glass)", e.body.seq, printable(&by));
            if let Some(why) = unsent {
                println!("the remote has not got it: {}", printable(&why));
                std::process::exit(3);
            }
        }
        Cmd::Snapshots => println!("{}", serde_json::to_string(&b.snapshots()?)?),
        Cmd::VerifyStore => {
            let c = b.verify_store()?;
            println!("{}", serde_json::to_string(&c)?);
            if !c.modified.is_empty() {
                std::process::exit(1);
            }
        }
        Cmd::SnapshotTake { approved_by } => println!("{}", serde_json::to_string(&b.snapshot_now(&approved_by)?)?),
        Cmd::SnapshotDelete { seq, approved_by } => println!("{}", serde_json::to_string(&b.delete_snapshot(seq, &approved_by)?)?),
        Cmd::RestoreData { seq, path, approved_by } => {
            let e = b.restore_data(seq, &path, &approved_by)?;
            println!("{}", serde_json::to_string(&e)?);
        }
        Cmd::FetchIncoming => {
            let id = b.fetch_incoming()?;
            if json {
                println!("{}", json!({ "incoming": id }));
            } else {
                println!("{}", id.as_deref().map_or("the remote has no incoming branch".to_string(), |c| format!("incoming: {c} (refs/torq/incoming)")));
            }
        }
        Cmd::Acknowledge { approved_by } => {
            let e = b.acknowledge_disagreements(&approved_by)?;
            println!("{}", serde_json::to_string(&e)?);
        }
        Cmd::RemoteClear { approved_by } => {
            let e = b.clear_remote_alarm(&approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("{} (ledger #{})", printable(&e.body.reason), e.body.seq);
        }
        Cmd::RemoteShow => {
            let r = b.remote()?;
            let last = r.as_ref().and_then(|r| r.last());
            let key = r.as_ref().is_some_and(|r| r.key().exists());
            if json {
                let alarm = r.as_ref().and_then(|r| r.alarm());
                println!("{}", json!({ "url": r.as_ref().map(|r| r.url.clone()), "key": key, "last": last, "alarm": alarm }));
            } else {
                match (&r, &last) {
                    (None, _) => println!("no remote: this machine's configuration and ledger are kept here only"),
                    (Some(r), None) => println!("remote {} (key: {}): nothing pushed yet", r.url, if key { "made" } else { "none yet" }),
                    (Some(r), Some(p)) => println!("remote {}: last push {} at {}{}", r.url, if p.ok { "went through" } else { "failed" }, p.at, p.error.as_deref().map(|e| format!(" — {}", printable(e))).unwrap_or_default()),
                }
            }
        }
        Cmd::Init => {
            b.init()?;
            if json {
                println!("{}", json!({ "initialised": s.repo, "ledger": b.ledger.path() }));
            } else {
                println!("initialised {} (ledger: {})", s.repo.display(), b.ledger.path().display());
            }
        }
        Cmd::Password { user, hash_file, approved_by } => {
            // (A file, as a diff is passed: the caller's standard input is sudo's, for its own password.)
            // A plain file, never followed through a link (it is opened as root).
            use std::os::unix::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(&hash_file).context("reading the hash (a plain file, not a link)")?;
            if !f.metadata()?.is_file() {
                anyhow::bail!("the hash file is not a plain file");
            }
            let mut hash = String::new();
            std::io::Read::read_to_string(&mut std::io::Read::take(f, 1024), &mut hash)?;
            let e = b.set_password(&user, hash.trim_end(), &approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("{} (ledger #{})", printable(&e.body.reason), e.body.seq);
        }
        Cmd::Setup => {
            let e = b.set_up()?;
            if json {
                return print_entry(&e);
            }
            println!("set up: change #{} (commit {})", e.body.seq, e.body.data["commit"].as_str().unwrap_or_default());
        }
        Cmd::Propose { diff, reason, actor, tainted, base } => {
            let e = b.propose_on(base.as_deref(), &diff, &reason, actor, tainted)?;
            if json {
                return print_entry(&e);
            }
            println!("proposal #{} · tier: {} · actor: {}{}", e.body.seq, e.body.tier, e.body.actor,
                if tainted { " · TAINTED" } else { "" });
            // (Names as the proposal wrote them: escaped as everything shown is, audit of 2026-11-01, L1.)
            println!("files: {}", printable(&e.body.data["files"].to_string()));
            println!("\n{}", printable_text(&b.proposal_text(&e, "diff").unwrap_or_default()));
            println!("approve with: torq-broker apply {} --approved-by <name>", e.body.seq);
        }
        Cmd::Show { seq } => {
            let mut e = b.ledger.get(seq)?;
            // A proposal's diff and what it does, from beside the ledger, as if they were in it: what a
            // person reads is what was proposed (N19).
            if e.body.kind == "proposal" {
                for what in ["diff", "effect"] {
                    if let Ok(t) = b.proposal_text(&e, what) {
                        e.body.data[what] = t.into();
                    }
                }
            }
            if json {
                return print_entry(&e);
            }
            println!("{}", printable_text(&serde_json::to_string_pretty(&e)?));
            if let Some(c) = e.body.data.get("commit").and_then(|c| c.as_str()) {
                println!("\n{}", printable_text(&b.repo.show(c)?));
            }
        }
        Cmd::Apply { seq, approved_by, confirm_tainted, confirm } => {
            let e = match confirm {
                Some(minutes) => b.apply_confirmed(seq, &approved_by, confirm_tainted, minutes)?,
                None => b.apply(seq, &approved_by, confirm_tainted)?,
            };
            if json {
                return print_entry(&e);
            }
            println!("applied #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
            if let Some(deadline) = e.body.data["confirm"]["deadline"].as_str() {
                println!("rolls back at {deadline} unless confirmed: torq-broker confirm --approved-by <name>");
            } else {
                println!("undo with: torq-broker revert {} --approved-by <name>", e.body.seq);
            }
        }
        Cmd::Confirm { approved_by } => {
            let e = b.confirm(&approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("confirmed #{} (ledger #{}): it is now the boot default", e.body.data["applied"], e.body.seq);
        }
        Cmd::ExpireConfirm { seq, reboot } => match (b.expire_confirm(seq, reboot)?, json) {
            (e, true) => println!("{}", json!({ "rolled_back": e })),
            (Some(e), false) => println!("{} (ledger #{})", e.body.reason, e.body.seq),
            (None, false) => println!("nothing is waiting for confirmation"),
        },
        Cmd::Revert { seq, approved_by } => {
            let e = b.revert(seq, &approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("reverted #{} as commit {} (ledger #{})", seq, e.body.data["commit"], e.body.seq);
        }
        Cmd::Reconcile { approved_by } => match (b.reconcile(&approved_by)?, json) {
            (e, true) => println!("{}", json!({ "reconciled": e })),
            (Some(e), false) => println!("{} (ledger #{})", e.body.reason, e.body.seq),
            (None, false) => println!("nothing to reconcile"),
        },
        Cmd::Repair { approved_by } => {
            let e = b.repair(&approved_by)?;
            if json {
                return print_entry(&e);
            }
            println!("{} (ledger #{})", printable(&e.body.reason), e.body.seq);
        }
        Cmd::Log { brief } => {
            for mut e in b.ledger.read_all()? {
                if brief {
                    if let Some(d) = e.body.data.as_object_mut() {
                        d.remove("diff");
                    }
                }
                if json {
                    print_entry(&e)?;
                } else {
                    // Ledger text comes from whoever proposed: never let it drive the terminal.
                    println!(
                        "#{:<4} {} {:<12} {:<9} {:<6}{} {}",
                        e.body.seq, printable(&e.body.ts), printable(&e.body.kind), printable(&e.body.tier), printable(&e.body.actor),
                        if e.body.tainted { " ⚠" } else { "" }, printable(&e.body.reason)
                    );
                }
            }
        }
        Cmd::Verify => {
            let n = b.ledger.verify()?;
            if json {
                println!("{}", json!({ "entries": n }));
            } else {
                println!("ledger OK: {n} entries, chain intact");
            }
        }
        Cmd::Status => {
            let s = b.status()?;
            let problems = s.problems();
            if json && s.busy.is_some() {
                // Mid-operation, pending.json exists and the tree may be half-applied: reporting
                // `interrupted` or `dirty` now would describe a running change as a broken one.
                println!("{}", json!({ "ok": false, "busy": s.busy, "problems": [] }));
                std::process::exit(75);
            }
            if json {
                println!(
                    "{}",
                    json!({
                        "ok": problems.is_empty() && s.busy.is_none(),
                        "busy": s.busy,
                        "problems": problems,
                        "ledger_entries": s.ledger.as_ref().ok(),
                        "ledger_error": s.ledger.as_ref().err(),
                        "head": s.head,
                        "ledgered_commit": s.ledgered_commit,
                        "running_system": s.running_system,
                        "next_boot": s.next_boot,
                        "ledgered_system": s.ledgered_system,
                        "checked_running_system": s.ledgered_system.is_some(),
                        "dirty": s.dirty,
                        "interrupted": s.interrupted,
                        "unrestored": s.unrestored,
                        "pending_confirm": s.pending_confirm,
                        "disagreements": s.disagreements,
                        "torn": s.torn,
                        "store_modified": s.store_modified,
                        "remote_alarm": s.remote_alarm.as_ref().map(|a| json!({ "at": a.at, "why": a.why })),
                    })
                );
                std::process::exit(if s.busy.is_some() { 75 } else if problems.is_empty() { 0 } else { 1 });
            }
            if let Some(who) = &s.busy {
                println!("a broker operation is running ({who}); not checked — try again when it is done");
                std::process::exit(75); // EX_TEMPFAIL: "busy" without parsing the text
            }
            let or = |v: &Option<String>, none: &str| v.clone().unwrap_or_else(|| none.to_string());
            println!("ledger          {}", match &s.ledger { Ok(n) => format!("OK, {n} entries"), Err(e) => e.clone() });
            println!("config repo     {}  (ledgered: {})", or(&s.head, "?"), or(&s.ledgered_commit, "none"));
            println!(
                "running system  {}  (ledgered: {})",
                or(&s.running_system, "?"),
                or(&s.ledgered_system, "not recorded yet: from the next change on")
            );
            if let Some(pc) = &s.pending_confirm {
                println!(
                    "waiting         change #{} (commit confirmed) rolls back at {} unless confirmed; \
                     the next boot starts the system before it",
                    pc.applied, pc.deadline
                );
            }
            if problems.is_empty() && s.ledgered_system.is_none() {
                println!("OK as far as recorded: the running system cannot be checked until a change records it");
            } else if problems.is_empty() {
                println!("OK: the machine is as the ledger says");
            } else {
                for p in &problems {
                    println!("PROBLEM: {p}");
                }
                std::process::exit(1);
            }
        }
    }
    Ok(())
}


/// Ledger text and proposed diffs are written by whoever proposed, the agent included, and must not
/// be able to make one thing look like another on a terminal (escape sequences, carriage returns,
/// bidi overrides — "Trojan Source" — invisible characters). Show those as visible escapes.
/// (The CLI's `history::clean` does the same; keep the two in step.)
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200b}'..='\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}' | '\u{061c}' | '\u{00ad}' | '\u{feff}'
            | '\u{fe00}'..='\u{fe0f}' | '\u{e0000}'..='\u{e007f}'
            | '\u{3164}' | '\u{ffa0}' | '\u{2800}' | '\u{115f}' | '\u{1160}')
}

/// One line: every hidden character escaped, newlines included.
fn printable(s: &str) -> String {
    s.chars().map(|c| if hidden(c) { c.escape_unicode().to_string() } else { c.to_string() }).collect()
}

/// Multi-line text (diffs, `git show`): keeps newlines and tabs, escapes every other hidden character.
fn printable_text(s: &str) -> String {
    s.chars()
        .map(|c| if hidden(c) && c != '\n' && c != '\t' { c.escape_unicode().to_string() } else { c.to_string() })
        .collect()
}

/// Pushes to the remote when dropped: after a command that writes the ledger, however it ended.
/// The broker, and how long its ledger was: a command that wrote nothing has nothing to send (a
/// refused proposal, over and over, was a way to keep the network and the lock busy: the audit of M3, M1).
struct PushAfter<'a>(&'a Broker, usize);

impl Drop for PushAfter<'_> {
    fn drop(&mut self) {
        if self.0.ledger.read_all().map_or(true, |l| l.len() != self.1) {
            let _ = self.0.sync_remote();
        }
    }
}
