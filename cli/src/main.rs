//! `torq`: the unprivileged command line. It never changes the system itself: it writes a
//! proposed change as a diff and hands it to `torq-broker`, which ledgers, applies and can undo it.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
// (In a test build, the test harness itself marks `main` `allow(dead_code)`, which `forbid` would
// reject: there it is `deny`. The real binary, which the Nix build compiles, forbids it, so an
// `allow(dead_code)` in the source never builds.)
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

mod agent;
mod at;
mod facts;
mod grammar;
mod near;
mod vocabulary;
mod history;
mod tree;
mod candidate;
mod session;
mod shell;
mod complete;
mod syntax;
mod filter;
mod nixos;
mod settings;
mod whole;
mod attach;
mod software;
mod release;
mod exclusive;
mod saved;
mod paint;
mod pkg;
mod run;
mod status;
mod ui;
mod nixerr;
mod setup;
mod install;
mod evalcache;
mod network;

use anyhow::{bail, Context, Result};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "/var/lib/torq/config";
/// The machine's broker: its socket (run.rs `SOCKET`, ADR-040), or — for root — the broker itself.
const BROKER: &str = "socket";
/// By absolute path: nothing earlier on PATH can stand in for the broker.
const DIRECT_BROKER: &str = "/run/current-system/sw/bin/torq-broker";
/// The configuration the broker builds, whatever the host is called (torq.broker.configName).
const CONFIG: &str = "torq";

/// How the shell reaches the machine: its config repo, and how to run the broker (it needs root).
/// `TORQ_CONFIG_REPO` and `TORQ_BROKER` override them, in a development build only (`cargo build`;
/// the release build ignores them), together (check_repo).
pub struct Cli {
    pub repo: PathBuf,
    pub broker: String,
}

/// The commands `torq` had before torqcli (ADR-016), and what each is now. None: not one of them.
fn retired(args: &[String]) -> Option<String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let now = match a.as_slice() {
        ["pkg", "add", ..] => "configure; set users <you> packages <name>; commit   (--system: set system packages <name>)",
        ["pkg", "remove", ..] => "configure; delete users <you> packages <name>; commit",
        ["pkg", "list", ..] => "show configuration users <you> packages   (and: show configuration system packages)",
        ["pkg", "search", ..] => "show packages available <term>",
        ["pkg", ..] => "configure; set users <you> packages <name>; commit",
        ["service", "list", ..] => "show services",
        ["service", "enable", ..] => "configure; set services <name>; commit",
        ["service", "disable", ..] => "configure; delete services <name>; commit",
        ["service", ..] => "show services",
        ["update", ..] => "request system update [all]; configure; commit",
        ["log", ..] => "show system ledger   (configurations: show system commit)",
        ["undo", ..] => "request system revert <change>   (a whole earlier configuration: rollback <n>; commit)",
        ["status", "--repair", ..] => "request system repair",
        ["status", ..] => "show system alarms",
        _ => return None,
    };
    Some(format!("`torq {}` is gone: TorqOS is driven by torqcli now (`torq` is torqcli). Instead: {now}", a[0]))
}

#[cfg(test)]
#[test]
fn the_old_commands_say_what_replaced_them() {
    let r = |s: &str| retired(&s.split_whitespace().map(String::from).collect::<Vec<_>>());
    assert!(r("pkg add htop").unwrap().contains("set users <you> packages <name>"));
    assert!(r("undo 23").unwrap().contains("request system revert <change>"));
    assert!(r("status --repair").unwrap().contains("request system repair"));
    assert!(r("log").unwrap().contains("show system ledger"));
    assert_eq!(r("show system alarms"), None, "a torqcli command is not a retired one");
    assert_eq!(r("-c show"), None);
}

fn main() {
    // `torq` and `torqcli` are one program: the shell (ADR-016).
    let args: Vec<String> = std::env::args().collect();
    if let Some(msg) = retired(&args[1..]) {
        eprintln!("{msg}");
        std::process::exit(2);
    }
    std::process::exit(shell::main(&args[1..]));
}

fn me() -> Result<String> {
    let out = Command::new("id").arg("-un").output().context("running id -un")?;
    let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || user.is_empty() {
        bail!("cannot tell who you are (id -un failed)");
    }
    Ok(user)
}

/// Search this machine's nixpkgs (`show packages available`): what matches, as (name, version,
/// description), sorted by name. Nix's own progress goes to the log (`log`), never the screen.
fn search(repo: &Path, terms: &[String], log: &Path) -> Result<Vec<(String, String, String)>> {
    let out = Command::new("nix")
        .args(["search", "--json", "--no-update-lock-file", "--inputs-from"])
        .arg(flake_ref(repo))
        .arg("nixpkgs")
        .args(terms)
        .output()
        .context("running nix search")?;
    let _ = fs::OpenOptions::new().append(true).open(log).and_then(|mut f| f.write_all(&out.stderr));
    // (No match is an error to nix search, with nothing on stdout.)
    if !out.status.success() && out.stdout.iter().all(u8::is_ascii_whitespace) {
        let why = last_error(&out.stderr);
        if why.contains("no results") {
            return Ok(vec![]);
        }
        bail!("searching nixpkgs: {why}");
    }
    search_results(&out.stdout, terms)
}

/// `nix search --json`'s answer as (name, version, description), sorted: each name as it is typed
/// (`set system packages <name>`), without nix's `legacyPackages.<system>.` in front.
/// nix search's answer as (name, version, description), closest first: the name itself, then a
/// set's package of that name, names beginning with the words, names holding them, then the rest
/// (found by their description) — each group by name.
fn search_results(json: &[u8], terms: &[String]) -> Result<Vec<(String, String, String)>> {
    let found: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(json).context("reading nix search's answer")?;
    let mut out: Vec<(String, String, String)> = found
        .into_iter()
        .map(|(attr, p)| {
            let name = attr.splitn(3, '.').nth(2).unwrap_or(&attr).to_string();
            let s = |k: &str| history::one_line(p[k].as_str().unwrap_or_default(), 300);
            (history::one_line(&name, 80), s("version"), s("description"))
        })
        .collect();
    let words: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    let rank = |name: &str| {
        let n = name.to_lowercase();
        let last = n.rsplit('.').next().unwrap_or(&n).to_string();
        if words.len() == 1 && n == words[0] {
            0
        } else if words.len() == 1 && last == words[0] {
            1
        } else if words.iter().all(|w| last.starts_with(w.as_str())) {
            2
        } else if words.iter().all(|w| n.contains(w.as_str())) {
            3
        } else {
            4
        }
    };
    out.sort_by(|a, b| rank(&a.0).cmp(&rank(&b.0)).then(a.0.cmp(&b.0)));
    Ok(out)
}

#[cfg(test)]
#[test]
fn search_results_are_names_as_typed() {
    let json = br#"{"legacyPackages.aarch64-linux.htop":{"pname":"htop","version":"3.4.1","description":"An interactive process viewer"},"legacyPackages.aarch64-linux.python3Packages.rich":{"pname":"rich","version":"13.9","description":"Rich text\nin the terminal"}}"#;
    assert_eq!(
        search_results(json, &["htop".to_string()]).unwrap(),
        [
            ("htop".to_string(), "3.4.1".to_string(), "An interactive process viewer".to_string()),
            ("python3Packages.rich".to_string(), "13.9".to_string(), "Rich text in the terminal".to_string()),
        ]
    );
    assert!(search_results(b"{}", &[]).unwrap().is_empty());
    // Closest first: the name itself, names beginning with it, names holding it, then the rest.
    let json = br#"{"x.y.chickenEggs.jq":{"version":"1","description":"jq for eggs"},"x.y.jq":{"version":"1.7","description":"JSON"},"x.y.gojq":{"version":"0.1","description":"jq in Go"},"x.y.jqp":{"version":"1","description":"TUI"},"x.y.yq":{"version":"3","description":"like jq"}}"#;
    let names: Vec<String> = search_results(json, &["jq".to_string()]).unwrap().into_iter().map(|r| r.0).collect();
    assert_eq!(names, ["jq", "chickenEggs.jq", "jqp", "gojq", "yq"]);
}

/// The config repo as a flake. `path:`, not a plain path: the repo belongs to root, and git refuses
/// to read a repository owned by someone else.
fn flake_ref(dir: &Path) -> String {
    format!("path:{}", dir.display())
}

/// A private (0700), unpredictable scratch directory: the broker, as root, reads the diff from it.
/// In XDG_RUNTIME_DIR (the user's own) if there is one, else /tmp; TMPDIR is not consulted.
pub(crate) fn scratch_dir() -> Result<tempfile::TempDir> {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
    tempfile::Builder::new().prefix("torq-").tempdir_in(base).context("creating a scratch directory")
}

/// Whether running the broker would stop to ask for a password: it runs through sudo, and sudo
/// cannot run it non-interactively (`sudo -n`). Checked against the broker itself, not `sudo -v`,
/// which follows the rules for *all* commands and could ask when the broker does not need it.
fn asks_for_password(cli: &Cli) -> bool {
    let words: Vec<&str> = cli.broker.split_whitespace().collect();
    match words.split_first() {
        Some((first, rest)) if *first == "sudo" || first.ends_with("/sudo") => !Command::new(first)
            .arg("-n")
            .args(rest)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
        _ => false,
    }
}

/// The owner SSH admits, and each key that will get in: (a warning for its options, what it is, where from).
type Admitted = (String, Vec<(String, String, String)>);

/// Who will be able to log in as the owner over SSH under an evaluated configuration (`admits`, as
/// `SSH_NIX` reads it: SSH on): the files sshd reads (`services.openssh.authorizedKeysFiles`) and the
/// keys declared in the config (`users.users.<owner>.openssh.authorizedKeys`, which NixOS writes to
/// /etc/ssh/authorized_keys.d/<owner> — taken from the config, not from what is running now). The
/// owner, and one description per key line, as `ssh-keygen -l` shows it, so the person approving
/// sees exactly whose keys will get in.
fn admitted(v: &serde_json::Value) -> Result<Admitted> {
    let owner = v["owner"].as_str().context("cannot read this machine's owner (torq.user)")?.to_string();
    if !pkg::valid_user(&owner) {
        bail!("'{}' is not a valid user name", history::one_line(&owner, 40));
    }
    // The owner's home and uid as the configuration being approved has them — not as the machine has
    // them now, which the change may alter (audit of 2026-10-31). A uid NixOS assigns itself (none
    // declared) is this machine's, if the user exists; only a key file named by uid (%U) needs it.
    if v["exists"] != true {
        bail!("the configuration being approved has no user '{owner}'");
    }
    let home = v["home"].as_str().filter(|h| !h.is_empty()).with_context(|| format!("the configuration gives '{owner}' no home"))?.to_string();
    let uid = match v["uid"].as_u64() {
        Some(u) => u.to_string(),
        None => {
            let out = Command::new("getent").args(["passwd", &owner]).output().context("running getent")?;
            let entry = String::from_utf8_lossy(&out.stdout).into_owned();
            entry.trim().split(':').nth(2).filter(|u| !u.is_empty()).map(String::from).unwrap_or_default()
        }
    };
    let (uid, home) = (uid.as_str(), home.as_str());

    let mut sources: Vec<(String, String)> = Vec::new(); // (where, text)
    for pattern in v["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()) {
        // Exactly the file NixOS writes the declared keys to: take those from the config instead.
        if pattern == "/etc/ssh/authorized_keys.d/%u" {
            let declared = v["declared"].as_array().into_iter().flatten().filter_map(|k| k.as_str());
            sources.push(("declared in the config".into(), declared.collect::<Vec<_>>().join("\n")));
            continue;
        }
        let file = pkg::expand_keys_pattern(pattern, home, &owner, uid)?;
        match fs::read_to_string(&file) {
            Ok(t) => sources.push((file, t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => bail!("cannot read {file}: {e} (run this as {owner})"),
        }
    }
    let mut keys = Vec::new();
    for (place, text) in sources {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some(id) = pkg::key_id(line)? {
                keys.push((pkg::key_options_note(line), id, place.clone()));
            }
        }
    }
    Ok((owner, keys))
}

/// The ledger, read through the broker (its state directory is root's): any account may read it.
fn read_ledger(cli: &Cli) -> Result<Vec<history::Entry>> {
    let (code, stdout) = run::read(broker(cli, &["log", "--json", "--brief"])?)?;
    if code != Some(0) {
        bail!("could not read the ledger (see above)");
    }
    history::parse(&String::from_utf8_lossy(&stdout))
}

/// The broker. Its repo is never passed: the real broker (as root) manages the machine's config
/// repo and nothing else, and `change` has refused to read a different one with it.
/// The real one is its socket (ADR-040): no sudo — who is asking is the kernel's answer, and a
/// system change asks the caller's password itself. Root runs the broker directly (as setup and
/// the machine's units do). A test broker (`TORQ_BROKER`, a development build) is a command, and
/// one through sudo where sudo would ask with no terminal to ask at is said so before anything
/// runs — not sudo's failure read as the broker's ("stopped unexpectedly").
fn broker(cli: &Cli, args: &[&str]) -> Result<run::Call> {
    use std::io::IsTerminal as _;
    if cli.broker == BROKER {
        if rustix::process::geteuid().is_root() {
            let mut c = Command::new(DIRECT_BROKER);
            c.args(args);
            return Ok(run::Call::Exec(c));
        }
        return Ok(run::Call::Socket(args.iter().map(|a| a.to_string()).collect()));
    }
    if !std::io::stdin().is_terminal() && asks_for_password(cli) {
        bail!("this goes through the broker, as root: sudo asks for your password, and there is no terminal to ask at (run it at one, or `sudo -v` first)");
    }
    let mut words = cli.broker.split_whitespace();
    let mut c = Command::new(words.next().unwrap_or("torq-broker"));
    c.args(words).args(args);
    Ok(run::Call::Exec(c))
}

/// A repo override only makes sense with a test broker: with the real one (anything run through
/// sudo, however it is spelled), a stray TORQ_CONFIG_REPO would mean evaluating one repo while
/// root changes another.
fn check_repo(cli: &Cli) -> Result<()> {
    let real_broker = cli.broker == BROKER || cli.broker.split_whitespace().any(|w| w == "sudo" || w.ends_with("/sudo"));
    if real_broker && cli.repo != Path::new(REPO) {
        bail!(
            "the config repo is set to {} (TORQ_CONFIG_REPO?), but the broker only manages {REPO}; \
             unset it",
            cli.repo.display()
        );
    }
    Ok(())
}

/// SSH as a configuration (`x`, its `config`) has it, in Nix: on, passwords (keys only unless
/// PasswordAuthentication is true: modules/services.nix), and — when on — what its keys are read
/// from: the owner (`torq.user`), their home and uid, the key files sshd reads, the keys declared
/// (`admitted` reads them). The owner is named in Nix, never spliced in from outside.
const SSH_NIX: &str = "x: let o = x.services.openssh; in { on = o.enable; passwords = o.settings.PasswordAuthentication != false; \
    admits = if !o.enable then null else let name = x.torq.user; u = x.users.users.${name} or null; \
    k = if u == null then { keys = []; keyFiles = []; } else u.openssh.authorizedKeys; in \
    { owner = name; files = o.authorizedKeysFiles; declared = k.keys ++ map builtins.readFile k.keyFiles; exists = u != null; \
    home = if u == null then null else u.home or null; uid = if u == null then null else u.uid or null; }; }";

/// What the check reads of the changed configuration (`c`), in Nix: its toplevel's drvPath (forced,
/// so its errors and warnings appear), whether it reads the tree, and SSH (SSH_NIX).
const EVALUATE_NIX: &str = "{ drv = builtins.unsafeDiscardStringContext c.system.build.toplevel.drvPath; readsTree = (c.torq.packages.configDir or null) != null; ssh = ssh c; }";

/// A name for what a kept result was read by: a hash of the Nix that read it (and the configuration
/// it reads), so a torqcli that reads it differently never takes an older one's for its own.
fn form_of(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    h.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// The form of SSH kept by commit (evalcache.rs).
pub(crate) fn ssh_form() -> String {
    format!("ssh-{}", form_of(&[CONFIG, SSH_NIX]))
}

/// The form of a check's result kept with the candidate (session.rs `check_key`).
pub(crate) fn check_form() -> String {
    format!("check-{}", form_of(&[CONFIG, SSH_NIX, EVALUATE_NIX]))
}

#[cfg(test)]
#[test]
fn a_kept_result_is_named_by_what_read_it() {
    assert_ne!(ssh_form(), check_form());
    assert_ne!(form_of(&[CONFIG, SSH_NIX]), form_of(&[CONFIG, &format!("{SSH_NIX} ")]), "any change to the Nix is another form");
    assert_ne!(form_of(&["ab", "c"]), form_of(&["a", "bc"]));
    assert_eq!(ssh_form(), ssh_form());
}

/// One `nix eval` of the configuration in `copy`: always `torq`, the one the broker builds (never the
/// host name), which the config repo must have; `what` is a Nix expression of its `config` (`c`) and
/// `ssh` (SSH_NIX). Its JSON, and Nix's standard error (its warnings). Nix's output goes to the log,
/// and with `live` to the terminal as it comes. `check`: the commit's check — never from Nix's
/// evaluation cache (a cached result says no warnings again), its error said as a check's.
fn eval_config(copy: &Path, what: &str, log: &Path, live: bool, check: bool) -> Result<(serde_json::Value, String)> {
    let apply = format!("cs: if !(cs ? {CONFIG}) then {{ missing = builtins.attrNames cs; }} else let c = cs.{CONFIG}.config; ssh = {SSH_NIX}; in {what}");
    let mut cmd = Command::new("nix");
    // As the broker builds it: a flake.lock that does not match flake.nix is an error, not re-locked.
    cmd.args(["eval", "--no-update-lock-file", "--json"]);
    if check {
        cmd.arg("--no-eval-cache");
    }
    cmd.arg(format!("{}#nixosConfigurations", flake_ref(copy))).arg("--apply").arg(apply);
    let out = run::output(&mut cmd, log, live).context("running nix eval")?;
    if !out.status.success() {
        // The whole of nix's explanation is in the log; the screen gets one clear sentence.
        let why = history::one_line(&last_error(&out.stderr), 300);
        if check {
            bail!("{}", pkg::explain_check_error(&why));
        }
        bail!("cannot read the SSH settings: {why}");
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).context("reading nix eval's answer")?;
    // The broker always builds `torq` (torq.broker.configName): checking any other configuration
    // here would approve something it never builds.
    if let Some(names) = v.get("missing") {
        bail!("the config repo has no configuration named `{CONFIG}` (it has {}); the broker builds only `{CONFIG}`", history::one_line(&names.to_string(), 200));
    }
    Ok((v, String::from_utf8_lossy(&out.stderr).into_owned()))
}

fn ssh_facts(v: &serde_json::Value) -> session::SshFacts {
    session::SshFacts { state: session::Ssh { on: v["on"] == true, passwords: v["passwords"] == true }, admits: v["admits"].clone() }
}

/// Evaluate the changed configuration in `copy` once, as the broker will build it: its toplevel (so
/// the same modules that will build it report unknown, unfree or unsupported packages, or a broken
/// update, with its warnings), SSH as it will be, and whether it reads the tree's files (its flake
/// passes `configDir` to torq.lib.mkSystem: without that, a change to them would commit and do
/// nothing).
fn evaluate(copy: &Path, log: &Path, live: bool) -> Result<session::Evaluated> {
    let what = EVALUATE_NIX;
    let (v, stderr) = eval_config(copy, what, log, live, true)?;
    Ok(session::Evaluated { warnings: nixerr::warnings(&stderr), ssh: ssh_facts(&v["ssh"]), reads_tree: v["readsTree"] == true })
}

/// SSH as the configuration in `copy` has it (the state before a change), evaluated.
fn ssh_state(copy: &Path, log: &Path, live: bool) -> Result<session::SshFacts> {
    let (v, _) = eval_config(copy, "ssh c", log, live, false)?;
    Ok(ssh_facts(&v))
}

/// The most useful line of a nix error — ours if there is one, else nix's last `error:` — as one
/// clean line: it can quote text from the config repo, so it never reaches the terminal raw.
fn last_error(stderr: &[u8]) -> String {
    let s = String::from_utf8_lossy(stderr);
    // A package check of TorqOS's own names the package, wherever Nix put it; otherwise the error
    // that stopped Nix, with its message wherever it is (nixerr.rs).
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| l.contains("torq.packages:"))
        .map(|l| l.trim_start_matches("error:").trim().to_string())
        .or_else(|| crate::nixerr::message(&s))
        .unwrap_or_else(|| s.trim().to_string());
    history::one_line(&line, 300)
}


#[cfg(test)]
mod lock_tests {
    /// Every evaluation of the configuration refuses to re-lock it (S4b; the audit of 2026-10-31
    /// found one that did): a flake.lock that does not match flake.nix is an error, as the broker
    /// builds it, never quietly re-locked. Any `nix eval`, `nix build` or `nix search` torqcli runs.
    #[test]
    fn no_evaluation_relocks() {
        for file in ["main.rs", "shell.rs", "pkg.rs", "status.rs", "session.rs"] {
            let text = std::fs::read_to_string(format!("{}/src/{file}", env!("CARGO_MANIFEST_DIR"))).unwrap();
            for (i, line) in text.lines().enumerate() {
                let runs = ["[\"eval\"", "[\"build\"", "[\"search\""].iter().any(|c| line.contains(c));
                if runs && !line.contains("--no-update-lock-file") {
                    panic!("{file}:{}: a nix command without --no-update-lock-file: {}", i + 1, line.trim());
                }
            }
        }
    }
}
