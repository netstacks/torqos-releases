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
mod saved;
mod paint;
mod pkg;
mod run;
mod status;
mod ui;
mod nixerr;
mod setup;

use anyhow::{bail, Context, Result};

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REPO: &str = "/var/lib/torq/config";
/// By absolute path: nothing earlier on PATH can stand in for sudo or the broker.
const BROKER: &str = "/run/wrappers/bin/sudo /run/current-system/sw/bin/torq-broker";
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

/// The machine's owner (`torq.user`) in the configuration at `copy`: the one user the catalog's SSH
/// admits.
fn owner(copy: &Path, host: &str) -> Result<String> {
    let out = Command::new("nix")
        // As the broker builds it: a flake.lock that does not match flake.nix is an error, not re-locked.
        .args(["eval", "--no-update-lock-file", "--raw"])
        .arg(format!("{}#nixosConfigurations.{host}.config.torq.user", flake_ref(copy)))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read this machine's owner: {}", last_error(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Who will be able to log in as the owner over SSH under the configuration being approved (`copy`,
/// already changed): the files sshd reads (`services.openssh.authorizedKeysFiles`) and the keys
/// declared in the config (`users.users.<owner>.openssh.authorizedKeys`, which NixOS writes to
/// /etc/ssh/authorized_keys.d/<owner> — taken from the config, not from what is running now).
/// One description per key line, as `ssh-keygen -l` shows it, so the person approving sees exactly
/// whose keys will get in.
fn owner_keys(copy: &Path, host: &str, owner: &str) -> Result<Vec<(String, String, String)>> {
    if !pkg::valid_user(owner) {
        bail!("'{owner}' is not a valid user name");
    }
    let out = Command::new("nix")
        .args(["eval", "--no-update-lock-file", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config", flake_ref(copy)))
        .arg("--apply")
        .arg(format!(
            "c: let u = c.users.users.\"{owner}\" or null; k = if u == null then {{ keys = []; keyFiles = []; }} else u.openssh.authorizedKeys; in \
             {{ files = c.services.openssh.authorizedKeysFiles; declared = k.keys ++ map builtins.readFile k.keyFiles; \
             exists = c.users.users ? \"{owner}\"; home = c.users.users.\"{owner}\".home or null; uid = c.users.users.\"{owner}\".uid or null; }}"
        ))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the SSH settings: {}", last_error(&out.stderr));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
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
            let out = Command::new("getent").args(["passwd", owner]).output().context("running getent")?;
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
        let file = pkg::expand_keys_pattern(pattern, home, owner, uid)?;
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
    Ok(keys)
}

/// The ledger, read through the broker (it is root-only).
fn read_ledger(cli: &Cli) -> Result<Vec<history::Entry>> {
    let out = broker(cli, &["log", "--json", "--brief"])?
        .stderr(Stdio::inherit())
        .output()
        .context("running the broker")?;
    if !out.status.success() {
        bail!("could not read the ledger (see above)");
    }
    history::parse(&String::from_utf8_lossy(&out.stdout))
}

/// The broker. Its repo is never passed: the real broker (as root) manages the machine's config
/// repo and nothing else, and `change` has refused to read a different one with it.
/// Reached through sudo, as the owner (their password): where sudo would ask and there is no
/// terminal to ask at (a script, `ssh host torqcli …`), said so before anything runs — not sudo's
/// failure read as the broker's ("stopped unexpectedly").
fn broker(cli: &Cli, args: &[&str]) -> Result<Command> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() && asks_for_password(cli) {
        bail!("this goes through the broker, as root: sudo asks for your password, and there is no terminal to ask at (run it at one, or `sudo -v` first)");
    }
    let mut words = cli.broker.split_whitespace();
    let mut c = Command::new(words.next().unwrap_or("torq-broker"));
    c.args(words).args(args);
    Ok(c)
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

/// The configuration to evaluate: always `torq`, the one the broker builds (never the host name),
/// and the config repo must have it.
fn config_name(repo: &Path) -> Result<String> {
    let out = Command::new("nix")
        .args(["eval", "--no-update-lock-file", "--json"])
        .arg(format!("{}#nixosConfigurations", flake_ref(repo)))
        .args(["--apply", "builtins.attrNames"])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the config repo: {}", last_error(&out.stderr));
    }
    let names: Vec<String> = serde_json::from_slice(&out.stdout)?;
    // The broker always builds `torq` (torq.broker.configName): checking any other configuration
    // here would approve something it never builds.
    if names.iter().any(|n| n == CONFIG) {
        Ok(CONFIG.to_string())
    } else {
        bail!(
            "{} has no configuration named `{CONFIG}` (it has {names:?}); the broker builds only `{CONFIG}`",
            repo.display()
        )
    }
}

/// Evaluate the whole system from the changed scratch copy: the same modules that will build it
/// report unknown, unfree or unsupported packages, or a broken update, before anything is proposed.
fn evaluate(copy: &Path, host: &str, log: &Path) -> Result<()> {
    let out = Command::new("nix")
        .args(["eval", "--no-update-lock-file", "--raw"])
        .arg(format!("{}#nixosConfigurations.{host}.config.system.build.toplevel.drvPath", flake_ref(copy)))
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        // The whole of nix's explanation goes to the log; the screen gets one clear sentence.
        let _ = fs::OpenOptions::new().append(true).open(log).and_then(|mut f| f.write_all(&out.stderr));
        bail!("{}", pkg::explain_check_error(&history::one_line(&last_error(&out.stderr), 300)));
    }
    Ok(())
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


/// Whether the configuration at `copy` reads the tree's files (its flake passes `configDir` to
/// torq.lib.mkSystem): without that, a change to them would commit and do nothing.
fn reads_tree(copy: &Path, host: &str) -> Result<bool> {
    let out = Command::new("nix")
        .args(["eval", "--no-update-lock-file", "--json"])
        .arg(format!("{}#nixosConfigurations.{host}.config.torq", flake_ref(copy)))
        .args(["--apply", "k: (k.packages.configDir or null) != null"])
        .output()
        .context("running nix eval")?;
    if !out.status.success() {
        bail!("cannot read the config repo: {}", last_error(&out.stderr));
    }
    Ok(serde_json::from_slice(&out.stdout)?)
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
