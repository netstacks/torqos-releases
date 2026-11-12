//! End-to-end: propose → apply → revert against a real git repo, with rebuild stubbed.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use torq_broker::{ledger::{Ledger, NewEntry}, policy::Actor, repo::Repo, Broker};
use std::fs;
use std::path::Path;
use std::process::Command;

/// A config repo with a few files, not yet initialised.
fn config_dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::create_dir_all(repo.join("policy")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ nightLight = false; }\n").unwrap();
    fs::write(repo.join("users/casey.nix"), "{ theme = \"dark\"; }\n").unwrap();
    fs::write(repo.join("policy/tiers.toml"), "agent_can_do_anything = false\n").unwrap();
    (dir, repo)
}

fn setup(rebuild: &str) -> (tempfile::TempDir, Broker) {
    let (dir, repo) = config_dir();
    // The agent may propose casey's packages (ADR-041: the owner's, by default), nothing else.
    let b = Broker::new(&repo, dir.path().join("state"), rebuild).with_agent_limits(&torq_broker::agent::Limits { users: vec!["casey".into()], settings: vec![] });
    b.init().unwrap();
    (dir, b)
}

fn write_diff(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    fs::write(&p, body).unwrap();
    p
}

const NIGHT_LIGHT: &str = "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = false; }\n+{ nightLight = true; }\n";

#[test]
fn propose_apply_revert() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Night light at sunset", Actor::User, false).unwrap();
    assert_eq!(p.body.tier, "system");

    let a = b.apply(p.body.seq, "casey", false).unwrap();
    assert!(a.body.data.get("rechecked_on").is_none(), "applied on its own base: no re-check to tell of");
    let cfg = b.repo.root().join("desktop.nix");
    assert!(fs::read_to_string(&cfg).unwrap().contains("true"));

    // Commit message carries the ledger trailer pointing at the applied entry.
    let log = Command::new("git").arg("-C").arg(b.repo.root()).args(["log", "-1", "--format=%B"]).output().unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);
    assert!(msg.contains(&format!("Ledger-Entry: {}", a.body.seq)), "{msg}");

    assert!(b.apply(p.body.seq, "casey", false).is_err(), "double apply must fail");

    b.revert(a.body.seq, "casey").unwrap();
    assert!(fs::read_to_string(&cfg).unwrap().contains("false"));
    assert!(b.revert(a.body.seq, "casey").is_err(), "double revert must fail");

    assert_eq!(b.ledger.verify().unwrap(), 4); // init, proposal, applied, reverted
}

#[test]
fn agent_cannot_touch_policy() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "p.diff",
        "--- /dev/null\n+++ b/policy/override.toml\n@@ -0,0 +1 @@\n+agent_can_do_anything = true\n",
    );
    let err = b.propose_on(Some(&b.repo.head().unwrap()), &d, "loosen my own permissions", Actor::Agent, false).unwrap_err();
    assert!(err.to_string().contains("forbidden"), "{err}");
}

#[test]
fn tainted_needs_explicit_confirmation() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "a web page suggested this", Actor::User, true).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    b.apply(p.body.seq, "casey", true).unwrap();
}

#[test]
fn failed_rebuild_resets_config() {
    let (dir, b) = setup("false");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "will fail to build", Actor::User, false).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    assert_eq!(b.repo.head().unwrap(), before);
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "apply_failed");
    b.ledger.verify().unwrap();
}

/// A rebuild can fail *after* activating the new system (switch-to-configuration exit 4).
/// The broker must then rebuild the reset config so the running system matches the repo.
#[test]
fn failed_rebuild_restores_previous_system() {
    let dir = tempfile::tempdir().unwrap();
    let calls = dir.path().join("calls.log");
    // Record what each rebuild saw; fail whenever the new setting is present.
    let rebuild = format!(
        "cat {{repo}}/desktop.nix >> {c}; ! grep -q true {{repo}}/desktop.nix",
        c = calls.display()
    );
    let (dir2, b) = setup(&rebuild);
    let d = write_diff(dir2.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "activates but fails", Actor::User, false).unwrap();
    let err = b.apply(p.body.seq, "casey", false).unwrap_err();

    let log = fs::read_to_string(&calls).unwrap_or_else(|e| panic!("{e}; apply error: {err:#}"));
    assert_eq!(log, "{ nightLight = true; }\n{ nightLight = false; }\n", "must rebuild the reset config");
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "apply_failed");
    assert_eq!(last.body.data["restored"], true);
}

#[test]
fn failed_revert_is_ledgered_and_restored() {
    let dir = tempfile::tempdir().unwrap();
    let flag = dir.path().join("fail");
    let rebuild = format!("! test -e {}", flag.display());
    let (dir2, b) = setup(&rebuild);
    let d = write_diff(dir2.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let head = b.repo.head().unwrap();

    fs::write(&flag, "").unwrap();
    assert!(b.revert(a.body.seq, "casey").is_err());
    assert_eq!(b.repo.head().unwrap(), head, "config must be reset to the applied state");
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.kind, "revert_failed");
    assert_eq!(last.body.data["restored"], false, "restore rebuild also failed, and must say so");
    b.ledger.verify().unwrap();
}

#[test]
fn user_tier_detected() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "t.diff",
        "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1,3 @@\n+[\n+  \"htop\"\n+]\n",
    );
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "install htop", Actor::Agent, false).unwrap();
    assert_eq!(p.body.tier, "user");
}

/// A new file at a user-tier path, as a diff.
fn new_user_file(path: &str, body: &str) -> String {
    let lines: Vec<String> = body.lines().map(|l| format!("+{l}\n")).collect();
    format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n{}", lines.len(), lines.concat())
}

/// User-tier files are data, by construction: a regular file holding exactly the shape its leaf
/// takes, small. Anything else at a user-tier path is refused, whoever proposes it.
#[test]
fn user_tier_files_must_be_json_data() {
    let (dir, b) = setup("true");
    let refused = |name: &str, diff: String| {
        let d = write_diff(dir.path(), name, &diff);
        let err = format!("{:#}", b.propose_on(Some(&b.repo.head().unwrap()), &d, "sneaky", Actor::Agent, false).unwrap_err());
        assert!(err.contains("must be a regular file holding"), "{name}: {err}");
    };
    refused("not-json.diff", new_user_file("users/casey/packages.json", "{ pkgs, ... }: { }"));
    refused("string.diff", new_user_file("users/casey/packages.json", "\"htop\""));
    refused("number.diff", new_user_file("users/casey/packages.json", "42"));
    // packages.json is a list of names: other JSON would only fail later, at the rebuild.
    refused("object.diff", new_user_file("users/casey/packages.json", "{}"));
    refused("list-of-numbers.diff", new_user_file("users/casey/packages.json", "[1]"));
    refused("list-of-objects.diff", new_user_file("users/casey/packages.json", "[{}]"));
    // At most 256 packages per user: one light change cannot start hours of builds.
    let many: Vec<String> = (0..257).map(|i| format!("\"p{i}\"")).collect();
    refused("too-many.diff", new_user_file("users/casey/packages.json", &format!("[{}]", many.join(","))));
    let big = format!("[\n{}\"x\"\n]", "\"padding-padding-padding-padding\",\n".repeat(2200));
    refused("big.diff", new_user_file("users/casey/packages.json", &big));
    // A symlink (mode 120000) pointing at a module, under a user-tier name.
    refused(
        "symlink.diff",
        "diff --git a/users/casey/packages.json b/users/casey/packages.json\nnew file mode 120000\n\
         --- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+../../desktop.nix\n\\ No newline at end of file\n"
            .to_string(),
    );
    // A symlink whose target text happens to be valid JSON: only the mode check can refuse it.
    refused(
        "symlink-json.diff",
        "diff --git a/users/casey/packages.json b/users/casey/packages.json\nnew file mode 120000\n\
         --- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[]\n\\ No newline at end of file\n"
            .to_string(),
    );
    // A list of names is fine, up to 256 of them.
    let ok = write_diff(dir.path(), "ok.diff", &new_user_file("users/casey/packages.json", "[\"htop\", \"ripgrep\"]"));
    assert_eq!(b.propose_on(Some(&b.repo.head().unwrap()), &ok, "two packages", Actor::Agent, false).unwrap().body.tier, "user");
    let most: Vec<String> = (0..256).map(|i| format!("\"p{i}\"")).collect();
    let d = write_diff(dir.path(), "most.diff", &new_user_file("users/casey/packages.json", &format!("[{}]", most.join(","))));
    assert_eq!(b.propose_on(Some(&b.repo.head().unwrap()), &d, "256 packages", Actor::Agent, false).unwrap().body.tier, "user");
}

/// Removing a user-tier file is a plain user-tier change: there is no content to check.
#[test]
fn user_tier_file_can_be_deleted() {
    let (dir, b) = setup("true");
    let add = write_diff(dir.path(), "add.diff", &new_user_file("users/casey/packages.json", "[\"htop\"]"));
    let p = b.propose(&add, "add", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let del = write_diff(
        dir.path(),
        "del.diff",
        "--- a/users/casey/packages.json\n+++ /dev/null\n@@ -1 +0,0 @@\n-[\"htop\"]\n",
    );
    assert_eq!(b.propose_on(Some(&b.repo.head().unwrap()), &del, "remove", Actor::Agent, false).unwrap().body.tier, "user");
}

/// `users/*.nix` is a full NixOS module (sudo rules, services, ...): changing it must never be
/// mistaken for a user-tier change just because it lives under `users/`.
#[test]
fn user_module_is_system_tier() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "t.diff", THEME);
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "light theme", Actor::User, false).unwrap();
    assert_eq!(p.body.tier, "system");
}

fn assert_agent_refused(b: &Broker, diff: &Path) {
    let err = b.propose_on(Some(&b.repo.head().unwrap()), diff, "sneaky", Actor::Agent, false).unwrap_err();
    assert!(err.to_string().contains("forbidden"), "{err:#}");
}

#[test]
fn agent_cannot_move_forbidden_file_away() {
    // numstat reports only the *new* name of a rename; the old (forbidden) path must count too.
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "mv.diff",
        "diff --git a/policy/tiers.toml b/users/junk.nix\nsimilarity index 100%\nrename from policy/tiers.toml\nrename to users/junk.nix\n",
    );
    assert_agent_refused(&b, &d);
}

#[test]
fn agent_cannot_delete_forbidden_file() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "rm.diff",
        "diff --git a/policy/tiers.toml b/policy/tiers.toml\ndeleted file mode 100644\n--- a/policy/tiers.toml\n+++ /dev/null\n@@ -1 +0,0 @@\n-agent_can_do_anything = false\n",
    );
    assert_agent_refused(&b, &d);
}

#[test]
fn agent_cannot_hide_forbidden_path_behind_quoting() {
    // git C-quotes unusual names ("policy/a\tb"), which defeats a plain prefix match.
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "q.diff", "--- /dev/null\n+++ \"b/policy/a\\tb\"\n@@ -0,0 +1 @@\n+evil\n");
    assert_agent_refused(&b, &d);
}

#[test]
fn relative_diff_path_is_resolved_against_cwd() {
    let (_dir, b) = setup("true");
    let here = tempfile::tempdir_in(".").unwrap();
    write_diff(here.path(), "nl.diff", NIGHT_LIGHT);
    let d = Path::new(here.path().file_name().unwrap()).join("nl.diff"); // relative to the cwd
    b.propose(&d, "relative path", Actor::User, false).unwrap();
}

#[test]
fn conflicting_revert_leaves_repo_usable() {
    let (dir, b) = setup("true");
    let d1 = write_diff(dir.path(), "a.diff", NIGHT_LIGHT);
    let p1 = b.propose(&d1, "night light", Actor::User, false).unwrap();
    let a1 = b.apply(p1.body.seq, "casey", false).unwrap();
    let d2 = write_diff(
        dir.path(),
        "b.diff",
        "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = true; }\n+{ nightLight = \"sunset\"; }\n",
    );
    let p2 = b.propose(&d2, "night light at sunset", Actor::User, false).unwrap();
    b.apply(p2.body.seq, "casey", false).unwrap();

    // Reverting the first change conflicts with the second.
    assert!(b.revert(a1.body.seq, "casey").is_err());
    b.repo.ensure_clean().expect("a failed revert must not leave the repo dirty");
    assert!(!b.repo.root().join(".git/REVERT_HEAD").exists());
    // ...and the broker keeps working.
    let d3 = write_diff(dir.path(), "c.diff", "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"dark\"; }\n+{ theme = \"light\"; }\n");
    b.propose(&d3, "light theme", Actor::User, false).unwrap();
}

#[test]
fn stale_lock_file_does_not_block() {
    // A broker killed mid-rebuild (Ctrl-C, power loss) must not wedge every later operation.
    let (dir, b) = setup("true");
    fs::write(dir.path().join("state/broker.lock"), "").unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    b.propose(&d, "after a crash", Actor::User, false).unwrap();
}

#[test]
fn concurrent_operation_is_refused() {
    let (dir, b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let err = b.propose(&d, "racing", Actor::User, false).unwrap_err();
    assert!(err.to_string().contains("another broker operation"), "{err:#}");
}

/// Run the real binary, so a crash is a real crash: the rebuild command SIGKILLs the broker.
fn run_cli(dir: &Path, rebuild: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_torq-broker"))
        .arg("--repo").arg(dir.join("config"))
        .arg("--state-dir").arg(dir.join("state"))
        .arg("--rebuild-cmd").arg(rebuild)
        .args(args)
        .output()
        .unwrap()
}

const CRASH: &str = "kill -KILL $PPID";

fn assert_killed(out: &std::process::Output) {
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(9), "broker should have been killed mid-rebuild: {out:?}");
}

fn trailers(b: &Broker) -> Vec<String> {
    let log = Command::new("git").arg("-C").arg(b.repo.root()).args(["log", "--format=%B", "main"]).output().unwrap();
    String::from_utf8_lossy(&log.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("Ledger-Entry: ").map(String::from))
        .collect()
}

/// A broker killed mid-rebuild leaves a commit that was never ledgered. The next operation must
/// discard it (it never completed), rebuild the last ledgered config, and ledger what happened.
#[test]
fn crash_mid_apply_is_reconciled_by_next_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();

    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));
    let orphan = b.repo.head().unwrap();
    assert_ne!(orphan, before, "the crash window leaves an unledgered commit behind");

    let a = b.apply(p.body.seq, "casey", false).unwrap();

    let entries = b.ledger.read_all().unwrap();
    let kinds: Vec<_> = entries.iter().map(|e| e.body.kind.as_str()).collect();
    assert_eq!(kinds, ["init", "proposal", "interrupted", "applied"]);
    let i = &entries[2];
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(i.body.data["restored"], true);
    // The orphan's trailer named the entry that now explains it, and it is kept, not lost to gc.
    assert_eq!(i.body.seq, 3);
    assert!(git(&b, &["log", "-1", "--format=%B", &orphan]).contains("Ledger-Entry: 3"));
    assert_eq!(git(&b, &["rev-parse", &format!("refs/torq/discarded/{orphan}")]).trim(), orphan);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "{ nightLight = false; }\n{ nightLight = true; }\n",
        "reconcile must rebuild the last ledgered config before the re-apply"
    );
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("true"));
    assert_eq!(trailers(&b), [a.body.seq.to_string(), "init".to_string()], "one commit per trailer");
    b.ledger.verify().unwrap();
}

#[test]
fn crash_mid_revert_is_reconciled_by_next_operation() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let applied_head = b.repo.head().unwrap();

    assert_killed(&run_cli(dir.path(), CRASH, &["revert", &a.body.seq.to_string(), "--approved-by", "casey"]));
    let orphan = b.repo.head().unwrap();
    assert_ne!(orphan, applied_head);

    // Reconcile puts the applied change back; the user can then undo it for real.
    b.revert(a.body.seq, "casey").unwrap();
    let kinds: Vec<_> = b.ledger.read_all().unwrap().into_iter().map(|e| e.body.kind).collect();
    assert_eq!(kinds, ["init", "proposal", "applied", "interrupted", "reverted"]);
    let i = &b.ledger.read_all().unwrap()[3];
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(i.body.data["commit"], applied_head.as_str());
    assert_eq!(git(&b, &["rev-parse", &format!("refs/torq/discarded/{orphan}")]).trim(), orphan);
    assert_eq!(i.body.data["restored"], true);
    assert_eq!(
        fs::read_to_string(&calls).unwrap(),
        "{ nightLight = true; }\n{ nightLight = true; }\n{ nightLight = false; }\n",
        "apply, then reconcile rebuilds the applied state, then the real revert"
    );
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
    b.ledger.verify().unwrap();
}

/// Killing the broker does not kill its rebuild: `nixos-rebuild` carries on and may still switch
/// the system. The rebuild's process tree must therefore keep holding the broker lock, so the next
/// operation waits for it instead of racing it.
#[test]
fn rebuild_outliving_a_killed_broker_keeps_the_lock() {
    use std::os::unix::process::ExitStatusExt;
    let (dir, b) = setup("true");
    let go = dir.path().join("go");
    let done = dir.path().join("late-rebuild-finished");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();

    // Kill the broker, then keep "rebuilding" (this shell survives its parent) until told to stop.
    let rebuild = format!(
        "kill -KILL $PPID; i=0; while [ ! -e {go} ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done; touch {done}",
        go = go.display(),
        done = done.display()
    );
    let status = Command::new(env!("CARGO_BIN_EXE_torq-broker"))
        .arg("--repo").arg(dir.path().join("config"))
        .arg("--state-dir").arg(dir.path().join("state"))
        .arg("--rebuild-cmd").arg(&rebuild)
        .args(["apply", &p.body.seq.to_string(), "--approved-by", "casey"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.signal(), Some(9));
    assert!(!done.exists(), "the rebuild should still be running");

    // propose takes the lock but never rebuilds, so the probe is about the lock only.
    let lock_error = |b: &Broker| b.propose(&d, "probe", Actor::User, false).err().map(|e| format!("{e:#}"));
    let locked = |b: &Broker| lock_error(b).is_some_and(|e| e.contains("another broker operation is running"));
    let err = lock_error(&b).unwrap();
    assert!(err.contains("another broker operation is running"), "the surviving rebuild must still hold the broker lock: {err}");
    assert!(err.contains("(sh)"), "the error must name the surviving rebuild shell: {err}");
    fs::write(&go, "").unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while locked(&b) {
        assert!(std::time::Instant::now() < deadline, "lock never released");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(done.exists(), "the lock must be held until the rebuild has finished");
}

/// First-boot setup killed after its switch: the machine boots the new system with no ledger. Its
/// record makes that system's first start (`init`) finish setup — change #1, the commit setup made —
/// and a history setup did not make is not taken over.
#[test]
fn setup_cut_short_after_its_switch_is_finished_at_the_next_start() {
    let (dir, repo) = config_dir();
    let state = dir.path().join("state");
    let setup = |rebuild: &str| {
        Command::new(env!("CARGO_BIN_EXE_torq-broker"))
            .arg("--repo").arg(&repo)
            .arg("--state-dir").arg(&state)
            .arg("--rebuild-cmd").arg(rebuild)
            .arg("--build-cmd").arg("echo /nix/store/built")
            .arg("setup")
            .output()
            .unwrap()
    };
    assert_killed(&setup(CRASH));
    fs::write(state.join("confirm-system"), "").unwrap(); // as nix build --out-link leaves it
    let b = Broker::new(&repo, &state, "true");
    assert!(b.ledger.read_all().unwrap().is_empty() && state.join("setup.json").exists(), "cut short: no history yet, its record kept");
    assert!(format!("{:#}", b.propose(&write_diff(dir.path(), "nl.diff", NIGHT_LIGHT), "x", Actor::User, false).unwrap_err()).contains("first-boot setup was cut short"));
    let head = b.repo.head().unwrap();
    b.init().unwrap();
    let entries = b.ledger.read_all().unwrap();
    assert_eq!(kinds(&b), ["init"]);
    assert_eq!(entries[0].body.data["commit"], head.as_str());
    assert_eq!(entries[0].body.data["interrupted"], true);
    assert!(!state.join("setup.json").exists());
    assert!(!state.join("confirm-system").exists(), "setup settled, so its build is not a GC root any more");
    b.init().unwrap();
    assert_eq!(kinds(&b), ["init"], "once");

    // Cut short again, and something else committed since: not taken over.
    let (dir, repo) = config_dir();
    let state = dir.path().join("state");
    let out = Command::new(env!("CARGO_BIN_EXE_torq-broker"))
        .arg("--repo").arg(&repo).arg("--state-dir").arg(&state)
        .arg("--rebuild-cmd").arg(CRASH).arg("--build-cmd").arg("echo /nix/store/built")
        .arg("setup").output().unwrap();
    assert_killed(&out);
    let b = Broker::new(&repo, &state, "true");
    fs::write(repo.join("desktop.nix"), "{ nightLight = true; }\n").unwrap();
    git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "outside"]);
    assert!(format!("{:#}", b.init().unwrap_err()).contains("more than setup's commit"));
    assert!(b.ledger.read_all().unwrap().is_empty());

    // On the image, a setup cut short starts again from nothing: its repo removed, its record goes too.
    fs::remove_dir_all(repo.join(".git")).unwrap();
    let b = Broker::new(&repo, &state, "true");
    let b = b.with_confirm_commands("touch {link}; echo /nix/store/built", "true", "true", "true");
    b.set_up().unwrap();
    assert_eq!(kinds(&b), ["init"]);
    assert!(!state.join("setup.json").exists());
    assert!(!state.join("confirm-system").exists(), "set up, so its build is not a GC root any more");
}

/// A reason is one line: it is the commit's subject, above the trailers the ledger is checked
/// against, which a line break in it could forge (audit of 2026-11-01, N4).
#[test]
fn a_reason_cannot_forge_a_trailer() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    for bad in ["Night light\n\nApproved-By: mallory\nLedger-Entry: 99", "x\u{1b}[2K", "", "   "] {
        let e = format!("{:#}", b.propose(&d, bad, Actor::User, false).unwrap_err());
        assert!(e.contains("the reason is one line of text"), "{bad:?}: {e}");
    }
    assert_eq!(kinds(&b), ["init"], "nothing proposed");
    b.propose(&d, "Night light — on", Actor::User, false).unwrap();
}

fn git(b: &Broker, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(b.repo.root()).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn kinds(b: &Broker) -> Vec<String> {
    b.ledger.read_all().unwrap().into_iter().map(|e| e.body.kind).collect()
}

const THEME: &str = "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"dark\"; }\n+{ theme = \"light\"; }\n";

/// propose is the agent's entry point and must not start a rebuild nobody approved: it refuses
/// until the interruption is reconciled, which the `reconcile` subcommand does.
#[test]
fn propose_refuses_until_reconcile_runs() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));

    let t = write_diff(dir.path(), "theme.diff", THEME);
    let err = format!("{:#}", b.propose_on(Some(&b.repo.head().unwrap()), &t, "Light theme", Actor::User, false).unwrap_err());
    assert!(err.contains("torq-broker reconcile"), "{err}");

    let out = run_cli(dir.path(), "true", &["reconcile", "--approved-by", "casey"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Interrupted apply of #2"), "{out:?}");
    let out = run_cli(dir.path(), "true", &["reconcile", "--approved-by", "casey"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing to reconcile"), "{out:?}");

    b.propose_on(Some(&b.repo.head().unwrap()), &t, "Light theme", Actor::User, false).unwrap();
    assert_eq!(kinds(&b), ["init", "proposal", "interrupted", "proposal"]);
    b.ledger.verify().unwrap();
}

/// If reconcile's own rebuild fails, that is ledgered and reported — and not retried forever.
#[test]
fn failed_reconcile_rebuild_is_ledgered_and_reported() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));

    let failing = Broker::new(dir.path().join("config"), dir.path().join("state"), "false");
    let err = format!("{:#}", failing.reconcile("casey").unwrap_err());
    assert!(err.contains("restore rebuild failed"), "{err}");
    let i = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(i.body.kind, "interrupted");
    assert_eq!(i.body.data["restored"], false);
    assert!(b.reconcile("casey").unwrap().is_none(), "already reconciled");
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"));
}

fn write_pending(dir: &Path, pending: serde_json::Value) {
    fs::write(dir.join("state/pending.json"), pending.to_string()).unwrap();
}

/// The broker died after ledgering the outcome but before removing pending.json: the change was
/// applied for real and must not be undone.
#[test]
fn ledgered_outcome_is_not_undone() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let head = b.repo.head().unwrap();
    let stale = serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": head,
        "approved_by": "casey", "tier": "system", "tainted": false,
    });

    // Both paths that settle a record: reconcile (apply, revert, the CLI) and propose/init.
    write_pending(dir.path(), stale.clone());
    assert!(b.reconcile("casey").unwrap().is_none());
    assert_eq!(b.repo.head().unwrap(), head);
    assert!(!dir.path().join("state/pending.json").exists());

    write_pending(dir.path(), stale);
    let t = write_diff(dir.path(), "theme.diff", THEME);
    b.propose(&t, "Light theme", Actor::User, false).unwrap();
    assert_eq!(b.repo.head().unwrap(), head);
    assert_eq!(kinds(&b), ["init", "proposal", "applied", "proposal"]);
    assert!(!dir.path().join("state/pending.json").exists());
}

/// Died after recording the operation but before committing: nothing to discard, but it still
/// rebuilds (it cannot tell this apart from dying inside a restore).
#[test]
fn crash_before_commit_still_rebuilds() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, b) = setup(&format!("cat {{repo}}/desktop.nix >> {}", calls.display()));
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    write_pending(dir.path(), serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": null,
        "approved_by": "casey", "tier": "system", "tainted": false,
    }));

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    assert_eq!(i.body.data["discarded_commit"], serde_json::Value::Null);
    assert_eq!(i.body.data["restored"], true);
    assert_eq!(fs::read_to_string(&calls).unwrap(), "{ nightLight = false; }\n");
    assert_eq!(b.repo.head().unwrap(), before);
}

/// `git apply --index` succeeds but `git commit` fails: the repo must be put back, not left dirty
/// (which would fail every later operation's clean check).
/// A PATH whose `git` fails for one subcommand (`commit`, `revert`) and is the real git otherwise.
fn failing_git(dir: &Path, sub: &str) -> String {
    let real = String::from_utf8(Command::new("sh").args(["-c", "command -v git"]).output().unwrap().stdout).unwrap();
    let bin = dir.join(format!("failing-{sub}"));
    fs::create_dir_all(&bin).unwrap();
    let script = format!("#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = {sub} ] && exit 1; done\nexec {} \"$@\"\n", real.trim());
    fs::write(bin.join("git"), script).unwrap();
    fs::set_permissions(bin.join("git"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
}

#[test]
fn failed_commit_leaves_repo_clean() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    // git commit fails (a repo hook cannot be the way: they never run).
    let out = run_cli_env(dir.path(), &[("PATH", &failing_git(dir.path(), "commit"))], &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]);
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(b.repo.head().unwrap(), before);
    b.repo.ensure_clean().unwrap();
    assert!(!dir.path().join("state/pending.json").exists());

    b.apply(p.body.seq, "casey", false).unwrap();
    assert_eq!(kinds(&b), ["init", "proposal", "applied"]);
}

/// Crash after an apply, then hand-written state, for reconcile's own guards.
fn crashed_apply() -> (tempfile::TempDir, Broker, String) {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert_killed(&run_cli(dir.path(), CRASH, &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]));
    (dir, b, before)
}

/// The commit exists but the broker died before recording it: reconcile must still record and keep it.
#[test]
fn unrecorded_commit_is_still_kept() {
    let (dir, b) = setup("true");
    let before = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    git(&b, &["apply", "--index", d.to_str().unwrap()]);
    git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "half-done apply"]);
    let orphan = b.repo.head().unwrap();
    write_pending(dir.path(), serde_json::json!({
        "op": "apply", "target": p.body.seq, "before": before, "ledger_len": 2, "commit": null,
        "approved_by": "casey", "tier": "system", "tainted": false,
    }));

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    assert_eq!(i.body.data["discarded_commit"], orphan.as_str());
    assert_eq!(git(&b, &["rev-parse", &format!("refs/torq/discarded/{orphan}")]).trim(), orphan);
    assert_eq!(b.repo.head().unwrap(), before);
}

/// HEAD does not descend from the recorded commit: something else rewrote the repo. Refuse
/// rather than reset to a commit that is not in its history.
#[test]
fn reconcile_refuses_when_before_is_not_an_ancestor() {
    let (dir, b, _) = crashed_apply();
    let head = b.repo.head().unwrap();
    let tree = git(&b, &["rev-parse", "HEAD^{tree}"]);
    let unrelated = git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit-tree", tree.trim(), "-m", "unrelated"]);
    let mut p: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("state/pending.json")).unwrap()).unwrap();
    p["before"] = unrelated.trim().into();
    write_pending(dir.path(), p);

    let err = format!("{:#}", b.reconcile("casey").unwrap_err());
    assert!(err.contains("does not descend"), "{err}");
    assert_eq!(b.repo.head().unwrap(), head, "must not reset");
    assert!(dir.path().join("state/pending.json").exists());
}

#[test]
fn init_refuses_until_reconciled() {
    let (_dir, b, _) = crashed_apply();
    let err = format!("{:#}", b.init().unwrap_err());
    assert!(err.contains("torq-broker reconcile"), "{err}");
}

#[test]
fn failed_revert_commit_leaves_repo_clean() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let applied = b.repo.head().unwrap();
    let out = run_cli_env(dir.path(), &[("PATH", &failing_git(dir.path(), "revert"))], &["revert", &a.body.seq.to_string(), "--approved-by", "casey"]);
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(b.repo.head().unwrap(), applied);
    b.repo.ensure_clean().unwrap();
    assert!(!dir.path().join("state/pending.json").exists());
}

/// An edit made in the repo outside the broker would be destroyed by the reset: keep it.
#[test]
fn reconcile_keeps_uncommitted_changes() {
    let (_dir, b, _) = crashed_apply();
    fs::write(b.repo.root().join("stray.nix"), "{ edited = \"by hand\"; }\n").unwrap();

    let i = b.reconcile("casey").unwrap().expect("reconciled");
    let kept = i.body.data["discarded_uncommitted"].as_str().expect("snapshot recorded").to_string();
    assert_eq!(git(&b, &["show", &format!("{kept}:stray.nix")]), "{ edited = \"by hand\"; }\n");
    assert_eq!(git(&b, &["rev-parse", &format!("refs/torq/discarded/{kept}")]).trim(), kept);
    assert_eq!(i.body.data["reconciled_by"], "casey");
    b.repo.ensure_clean().unwrap();
}

/// On a broken chain the final append would fail after a full reset and rebuild, on every retry.
#[test]
fn reconcile_refuses_on_a_broken_ledger_before_touching_anything() {
    let (dir, b, _) = crashed_apply();
    let head = b.repo.head().unwrap();
    let ledger = dir.path().join("state/ledger.jsonl");
    fs::write(&ledger, fs::read_to_string(&ledger).unwrap().replace("Night light", "Day light")).unwrap();
    let calls = dir.path().join("calls.log");
    let logging = Broker::new(dir.path().join("config"), dir.path().join("state"), format!("touch {}", calls.display()));

    let err = format!("{:#}", logging.reconcile("casey").unwrap_err());
    assert!(err.contains("the ledger is broken"), "{err}");
    assert_eq!(b.repo.head().unwrap(), head);
    assert!(!calls.exists(), "must not rebuild");
}

/// git children do not hold the broker lock, so an index.lock may belong to a git that is still
/// running: only a lock nobody has open may be removed.
#[test]
fn live_index_lock_is_refused_stale_one_removed() {
    let (_dir, b, _) = crashed_apply();
    let index_lock = b.repo.index_lock();
    fs::write(&index_lock, "").unwrap();
    // Open the lock, then become `sleep` (exec: one process, so killing it releases the fd). Ready once
    // it is `sleep`: the lock is open by then, and the name is the one the refusal must give.
    let mut holder = Command::new("sh")
        .arg("-c")
        .arg("exec 3<\"$0\"; exec sleep 30")
        .arg(&index_lock)
        .spawn()
        .unwrap();
    let comm = format!("/proc/{}/comm", holder.id());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fs::read_to_string(&comm).unwrap_or_default().trim() != "sleep" {
        assert!(std::time::Instant::now() < deadline, "holder never became sleep with the lock open");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let err = format!("{:#}", b.reconcile("casey").unwrap_err());
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert!(err.contains("git is still running") && err.contains("(sleep)"), "{err}");
    assert!(index_lock.exists());

    b.reconcile("casey").unwrap().expect("stale lock removed, reconciled");
    assert!(!index_lock.exists());
}

/// A broker whose "running system" is a symlink the test controls, like /run/current-system.
fn setup_with_system() -> (tempfile::TempDir, Broker, std::path::PathBuf) {
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile")); // not this machine's real one
    b.init().unwrap(); // records the fake system as the baseline, as on a real machine
    (dir, b, link)
}

fn switch_to(link: &Path, target: &str) {
    fs::remove_file(link).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Every change records the system it switched to, so drift from the ledger can be detected.
#[test]
fn changes_record_the_running_system() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    assert_eq!(a.body.data["system"], "/nix/store/aaa-system-1");
    switch_to(&link, "/nix/store/bbb-system-2");
    let r = b.revert(a.body.seq, "casey").unwrap();
    assert_eq!(r.body.data["system"], "/nix/store/bbb-system-2");
}

/// Something switched the machine outside the broker (today's incident): status says so.
#[test]
fn status_detects_a_system_switched_outside_the_broker() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let s = b.status().unwrap();
    assert!(s.problems().is_empty(), "{:?}", s.problems());

    switch_to(&link, "/nix/store/zzz-nixos-system-nixos");
    let s = b.status().unwrap();
    let problems = s.problems().join("\n");
    assert!(problems.contains("running system") && problems.contains("zzz-nixos-system-nixos"), "{problems}");
}

/// A commit in the config repo that the ledger does not know about.
#[test]
fn status_detects_an_unledgered_commit() {
    let (dir, b, _) = setup_with_system();
    fs::write(b.repo.root().join("desktop.nix"), "{ nightLight = true; }\n").unwrap();
    git(&b, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", "edited behind the broker's back"]);
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("config repo"), "{problems}");
    let _ = dir;
}

#[test]
fn status_reports_an_interrupted_operation_and_a_broken_ledger() {
    let (dir, b, _) = crashed_apply();
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("interrupted"), "{problems}");
    let ledger = dir.path().join("state/ledger.jsonl");
    fs::write(&ledger, fs::read_to_string(&ledger).unwrap().replace("Night light", "Day light")).unwrap();
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("ledger"), "{problems}");
}

/// Mid-apply, the system has switched but the ledger entry is not written yet: comparing then would
/// report drift that is not there. status must say an operation is running instead.
#[test]
fn status_does_not_compare_during_an_operation() {
    let (dir, b, link) = setup_with_system();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    switch_to(&link, "/nix/store/ccc-being-switched-to");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();

    let s = b.status().unwrap();
    assert!(s.busy.is_some(), "{s:?}");
    assert!(s.problems().is_empty(), "{:?}", s.problems());
    drop(held);
    assert!(!b.status().unwrap().problems().is_empty(), "once the lock is free, the drift is reported");
}

/// A rebuild that failed and could not restore leaves the machine in an unknown state; status must
/// say so even though it records whatever is running then.
#[test]
fn status_reports_a_failed_restore() {
    let (dir, _, link) = setup_with_system();
    let b = Broker::new(dir.path().join("config"), dir.path().join("state"), "false")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("could not restore"), "{problems}");
}

/// init records the running system, so drift is detectable from the first boot, not only after
/// the first change.
#[test]
fn init_records_the_running_system() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("flake.nix"), "{ }\n").unwrap();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    b.init().unwrap();
    assert_eq!(b.ledger.read_all().unwrap()[0].body.data["system"], "/nix/store/aaa-system-1");
    switch_to(&link, "/nix/store/zzz-elsewhere");
    assert!(b.status().unwrap().problems().join("\n").contains("running system"));
}

#[test]
fn status_reports_an_unreadable_or_dirty_repo() {
    let (_dir, b, _) = setup_with_system();
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    assert!(b.status().unwrap().problems().join("\n").contains("uncommitted"));
    fs::remove_dir_all(b.repo.root().join(".git")).unwrap();
    assert!(b.status().unwrap().problems().join("\n").contains("cannot be read"));
}

/// `nixos-rebuild boot` / `nix-env --set` outside the broker change only the next boot.
#[test]
fn status_reports_a_next_boot_changed_outside_the_broker() {
    let (dir, b, _) = setup_with_system();
    let profiles = dir.path().join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", profiles.join("system-1-link")).unwrap();
    std::os::unix::fs::symlink("system-1-link", profiles.join("system")).unwrap(); // relative, like NixOS
    let b = b.with_system_profile(profiles.join("system"));
    assert!(b.status().unwrap().problems().is_empty(), "{:?}", b.status().unwrap().problems());
    std::os::unix::fs::symlink("/nix/store/bbb-stock-nixos", profiles.join("system-2-link")).unwrap();
    switch_to(&profiles.join("system"), "system-2-link");
    let problems = b.status().unwrap().problems().join("\n");
    assert!(problems.contains("next boot") && problems.contains("bbb-stock-nixos"), "{problems}");
}

/// A status check holds the lock for milliseconds; an operation that collides with it must wait,
/// not fail.
#[test]
fn operation_waits_briefly_for_the_lock() {
    let (dir, b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(held);
    });
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    b.propose(&d, "waited", Actor::User, false).unwrap();
    release.join().unwrap();
}

/// The CLI signals "busy, not checked" with its own exit code (75), not just text.
#[test]
fn status_cli_exits_75_while_busy() {
    let (dir, _b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let out = run_cli(dir.path(), "true", &["status"]);
    assert_eq!(out.status.code(), Some(75), "{out:?}");
}

/// The broker's own `log` must not let a proposal's reason drive the terminal; `--brief` drops
/// the diffs.
#[test]
fn log_neutralises_control_characters_and_brief_drops_diffs() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    // (Control characters are refused at propose now; hidden ones that are not — a bidi override, a
    // zero-width space — still reach the ledger, and entries from before may hold either.)
    b.propose_on(Some(&b.repo.head().unwrap()), &d, "x\u{202e}#99 applied fake\u{200b}", Actor::User, false).unwrap();
    let out = run_cli(dir.path(), "true", &["log"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains('\u{202e}') && !text.contains('\u{200b}'), "{text:?}");
    assert_eq!(text.lines().count(), 2, "one line per entry: {text}");
    let brief = String::from_utf8(run_cli(dir.path(), "true", &["log", "--json", "--brief"]).stdout).unwrap();
    assert!(!brief.contains("\"diff\":"), "{brief}");
    let full = String::from_utf8(run_cli(dir.path(), "true", &["log", "--json"]).stdout).unwrap();
    // Diffs are beside the ledger now (N19): the entry names them by hash, and `show` reads them.
    assert!(full.contains("\"diff_sha256\":") && !full.contains("\"diff\":"), "{full}");
    let shown = String::from_utf8(run_cli(dir.path(), "true", &["--json", "show", "2"]).stdout).unwrap();
    assert!(shown.contains("\"diff\":"), "{shown}");
}

/// `show` is where a person reviews an agent's change: a bidi override in the diff ("Trojan Source")
/// or an escape sequence must be shown, not interpreted.
#[test]
fn show_and_propose_neutralise_hidden_characters_in_diffs() {
    let (dir, b) = setup("true");
    let d = write_diff(
        dir.path(),
        "t.diff",
        "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = false; }\n+{ nightLight = true; /* \u{202e} } \u{2066} */ \u{1b}[2K}\n",
    );
    let head = b.repo.head().unwrap();
    let out = run_cli(dir.path(), "true", &["propose", "--diff", d.to_str().unwrap(), "--reason", "r", "--actor", "user", "--base", &head]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(r"\u{202e}") && text.contains(r"\u{1b}"), "{text}");
    assert!(!text.contains('\u{202e}') && !text.contains('\u{1b}'), "{text:?}");
    let shown = String::from_utf8(run_cli(dir.path(), "true", &["show", "2"]).stdout).unwrap();
    assert!(!shown.contains('\u{202e}') && !shown.contains('\u{1b}') && shown.contains('\n'), "{shown:?}");
    let _ = b;
}

/// ADR-014: the CLI reads the broker's results as data, never its human text.
#[test]
fn json_output_for_every_command_the_cli_uses() {
    let (dir, _b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let json = |args: &[&str]| -> (Option<i32>, serde_json::Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = run_cli(dir.path(), "true", &all);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {out:?}"));
        (out.status.code(), v)
    };

    let (code, p) = json(&["propose", "--diff", d.to_str().unwrap(), "--reason", "Night light"]);
    assert_eq!(code, Some(0));
    assert_eq!((p["seq"].as_u64(), p["kind"].as_str(), p["tier"].as_str()), (Some(2), Some("proposal"), Some("system")));
    assert_eq!(p["data"]["files"], serde_json::json!(["desktop.nix"]));

    let (_, a) = json(&["apply", "2", "--approved-by", "casey"]);
    assert_eq!((a["seq"].as_u64(), a["kind"].as_str(), a["data"]["proposal"].as_u64()), (Some(3), Some("applied"), Some(2)));

    let (code, s) = json(&["status"]);
    assert_eq!(code, Some(0), "{s}");
    assert_eq!(s["ok"], true);
    assert_eq!(s["problems"], serde_json::json!([]));
    assert_eq!(s["ledger_entries"].as_u64(), Some(3));

    let (_, r) = json(&["revert", "3", "--approved-by", "casey"]);
    assert_eq!((r["kind"].as_str(), r["data"]["applied"].as_u64()), (Some("reverted"), Some(3)));

    // Refusals are data too, with a non-zero exit.
    let (code, e) = json(&["revert", "3", "--approved-by", "casey"]);
    assert_eq!(code, Some(1));
    assert!(e["error"].as_str().unwrap().contains("already reverted"), "{e}");
}

/// stdout carries the result and nothing else: the rebuild's own output (activation scripts print
/// to stdout through systemd-run --pipe) goes to stderr, or a JSON consumer could not parse a
/// change that was already applied.
#[test]
fn rebuild_output_never_mixes_into_the_json_result() {
    let (dir, _b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let noisy = "echo REBUILD-STDOUT-NOISE";
    run_cli(dir.path(), noisy, &["propose", "--diff", d.to_str().unwrap(), "--reason", "r"]);
    let out = run_cli(dir.path(), noisy, &["--json", "apply", "2", "--approved-by", "casey"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.lines().count(), 1, "stdout is exactly the result: {stdout:?}");
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["kind"], "applied");
    assert!(String::from_utf8_lossy(&out.stderr).contains("REBUILD-STDOUT-NOISE"), "still visible, on stderr");
}

/// "Busy" is one contract: exit 75 (and "busy": true) from any command that finds another
/// operation running, not only status.
#[test]
fn every_command_exits_75_when_busy() {
    let (dir, _b) = setup("true");
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let out = run_cli(dir.path(), "true", &["--json", "propose", "--diff", d.to_str().unwrap(), "--reason", "r"]);
    assert_eq!(out.status.code(), Some(75), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["busy"], true, "{v}");
    let out = run_cli(dir.path(), "true", &["--json", "status"]);
    assert_eq!(out.status.code(), Some(75));
}

/// status --json carries the facts as fields, so the CLI never matches English problem text.
#[test]
fn status_json_has_facts_as_fields() {
    let (dir, b, _) = crashed_apply();
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    let out = run_cli(dir.path(), "true", &["--json", "status"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["dirty"], true, "{v}");
    assert_eq!(v["interrupted"]["op"], "apply", "{v}");
    assert_eq!(v["interrupted"]["target"], 2, "{v}");
    assert!(v.get("unrestored").is_some() && v.get("checked_running_system").is_some(), "{v}");
    let out = run_cli(dir.path(), "true", &["--json", "reconcile", "--approved-by", "casey"]);
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["reconciled"]["kind"], "interrupted", "{r}");
}

/// While an operation runs, status --json says busy and nothing else: mid-apply pending.json exists,
/// so `interrupted` would describe a running change as a broken one.
#[test]
fn status_json_reports_nothing_else_while_busy() {
    let (dir, _b, _) = crashed_apply(); // pending.json exists, as during a running apply
    let held = fs::OpenOptions::new().write(true).open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let out = run_cli(dir.path(), "true", &["--json", "status"]);
    assert_eq!(out.status.code(), Some(75));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["busy"].is_string() && v.get("interrupted").is_none() && v.get("dirty").is_none(), "{v}");
}

/// `repair`: the person's way back to the ledgered system after drift (ADR-012: never automatic).
/// A machine switched outside the broker is rebuilt from its last ledgered config, and that is
/// ledgered, recording the system it is now running.
#[test]
fn repair_returns_a_drifted_machine_to_its_ledgered_system() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    // The "rebuild" switches the link back to the ledgered system, as nixos-rebuild switch would.
    let rebuild = format!("echo rebuilt >> {}; ln -sfn /nix/store/aaa-system-1 {}", calls.display(), link.display());
    let b = Broker::new(&repo, dir.path().join("state"), rebuild).with_current_system(&link).with_system_profile(dir.path().join("none"));
    b.init().unwrap();
    switch_to(&link, "/nix/store/zzz-switched-outside");
    assert!(!b.status().unwrap().problems().is_empty());

    let e = b.repair("casey").unwrap();
    assert_eq!(e.body.kind, "repaired");
    assert_eq!(e.body.data["system"], "/nix/store/aaa-system-1");
    assert_eq!(fs::read_to_string(&calls).unwrap(), "rebuilt\n");
    assert!(b.status().unwrap().problems().is_empty(), "{:?}", b.status().unwrap().problems());
    b.ledger.verify().unwrap();
}

/// A failed restore leaves the machine unknown; repair rebuilds and clears it.
#[test]
fn repair_clears_a_failed_restore() {
    let (dir, _, link) = setup_with_system();
    let failing = Broker::new(dir.path().join("config"), dir.path().join("state"), "false")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = failing.propose(&d, "Night light", Actor::User, false).unwrap();
    assert!(failing.apply(p.body.seq, "casey", false).is_err());
    let ok = Broker::new(dir.path().join("config"), dir.path().join("state"), "true")
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"));
    assert!(!ok.status().unwrap().problems().is_empty());
    ok.repair("casey").unwrap();
    assert!(ok.status().unwrap().problems().is_empty(), "{:?}", ok.status().unwrap().problems());
}

/// The config repo itself changed outside the broker: a rebuild would build that, so repair refuses
/// and says why. An interrupted operation is finished by reconciling, which is the repair.
#[test]
fn repair_refuses_a_changed_repo_and_reconciles_an_interruption() {
    let (_dir, b) = setup("true");
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    let err = format!("{:#}", b.repair("casey").unwrap_err());
    assert!(err.contains("changed outside the broker"), "{err}");
    fs::remove_file(b.repo.root().join("stray.nix")).unwrap();

    let (_dir2, b2, _) = crashed_apply();
    let e = b2.repair("casey").unwrap();
    assert_eq!(e.body.kind, "interrupted", "an interruption is repaired by reconciling it");
}

/// A rebuild that "succeeds" while something else decides what runs (an outside nixos-rebuild
/// finishing during the repair) must not be recorded as a repair: the running system and the next
/// boot are checked, and the drift that was found is ledgered, not erased.
#[test]
fn repair_does_not_vouch_for_a_system_it_did_not_switch_to() {
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    let profiles = dir.path().join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", profiles.join("system")).unwrap();
    // The rebuild exits 0, but leaves the running system on the outside one.
    let b = Broker::new(&repo, dir.path().join("state"), "true").with_current_system(&link).with_system_profile(profiles.join("system"));
    b.init().unwrap();
    switch_to(&link, "/nix/store/zzz-outside");
    let err = format!("{:#}", b.repair("casey").unwrap_err());
    assert!(err.contains("did not take"), "{err}");
    let e = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((e.body.kind.as_str(), &e.body.data["restored"]), ("repaired", &serde_json::json!(false)));
    assert_eq!(e.body.data["drift"]["running_system"], "/nix/store/zzz-outside", "the drift is ledgered");
    assert!(!b.status().unwrap().problems().is_empty(), "status must not turn happy");
}

/// Next-boot drift (nixos-rebuild boot outside the broker) is repaired by a switch.
#[test]
fn repair_fixes_next_boot_drift_and_refusals_rebuild_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = tmp.path().join("calls.log");
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    let profile = dir.path().join("system-profile");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &profile).unwrap();
    let rebuild = format!("echo rebuilt >> {}; ln -sfn /nix/store/aaa-system-1 {}", calls.display(), profile.display());
    let b = Broker::new(&repo, dir.path().join("state"), rebuild).with_current_system(&link).with_system_profile(&profile);
    b.init().unwrap();

    let err = format!("{:#}", b.repair("casey").unwrap_err());
    assert!(err.contains("nothing to repair"), "{err}");
    switch_to(&profile, "/nix/store/bbb-boot-outside");
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    assert!(b.repair("casey").is_err());
    assert!(!calls.exists(), "refusals rebuild nothing");
    fs::remove_file(b.repo.root().join("stray.nix")).unwrap();

    let e = b.repair("casey").unwrap();
    assert_eq!(e.body.data["drift"]["next_boot"], "/nix/store/bbb-boot-outside");
    assert_eq!(e.body.data["commit"].as_str(), b.repo.head().ok().as_deref(), "the ledgered commit, unchanged");
    assert!(b.status().unwrap().problems().is_empty(), "{:?}", b.status().unwrap().problems());
}

/// A boot profile that vanishes during the repair is not something the repair can vouch for.
#[test]
fn repair_does_not_vouch_when_the_boot_profile_vanishes() {
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    let profile = dir.path().join("system-profile");
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &link).unwrap();
    std::os::unix::fs::symlink("/nix/store/aaa-system-1", &profile).unwrap();
    let rebuild = format!("ln -sfn /nix/store/aaa-system-1 {}; rm -f {}", link.display(), profile.display());
    let b = Broker::new(&repo, dir.path().join("state"), rebuild).with_current_system(&link).with_system_profile(&profile);
    b.init().unwrap();
    switch_to(&link, "/nix/store/zzz-outside");
    let err = format!("{:#}", b.repair("casey").unwrap_err());
    assert!(err.contains("did not take"), "{err}");
    let e = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(e.body.data["restored"], serde_json::json!(false));
}

/// A change prepared against one configuration must not be applied on top of another: the commit
/// in between would be silently undone or mixed (review H4). With a base, the broker refuses.
#[test]
fn proposal_based_on_current_config_applies() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose_on(Some(&head), &d, "Night light", Actor::User, false).unwrap();
    assert_eq!(p.body.data["base"], head.as_str());
    b.apply(p.body.seq, "casey", false).unwrap();
}

#[test]
fn proposal_based_on_a_stale_config_is_refused_at_propose() {
    let (dir, b) = setup("true");
    let stale = b.repo.head().unwrap();
    let t = write_diff(dir.path(), "theme.diff", THEME);
    let other = b.propose(&t, "someone else's change", Actor::User, false).unwrap();
    b.apply(other.body.seq, "alice", false).unwrap();

    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let err = format!("{:#}", b.propose_on(Some(&stale), &d, "Night light", Actor::User, false).unwrap_err());
    assert!(err.contains("changed since"), "{err}");
}

/// ADR-053 (a): another change lands between my propose and my apply, in another file (a desktop saved
/// meanwhile). Re-checked on the new head, the change does exactly what was shown: applied there, and
/// the history says it was re-checked on a moved configuration.
#[test]
fn proposal_whose_base_moved_by_another_file_applies_rechecked() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let mine = b.propose_on(Some(&head), &d, "Night light", Actor::User, false).unwrap();
    let t = write_diff(dir.path(), "theme.diff", THEME);
    let other = b.propose(&t, "someone else's change", Actor::User, false).unwrap();
    b.apply(other.body.seq, "alice", false).unwrap();
    let moved = b.repo.head().unwrap();
    assert_ne!(moved, head);

    let a = b.apply(mine.body.seq, "casey", false).unwrap();
    assert_eq!(a.body.data["rechecked_on"], serde_json::json!(moved), "the ledger says it was re-checked, and on what");
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("true"));
    assert!(fs::read_to_string(b.repo.root().join("users/casey.nix")).unwrap().contains("light"), "the change in between kept");
    assert_eq!(git(&b, &["rev-parse", "HEAD^"]).trim(), moved, "built on the current head");
    b.ledger.verify().unwrap();
}

/// ADR-053 (c): the commit in between changed the same file. The diff still applies, to the same file
/// and tier, but what it does there is no longer what was shown: refused, nothing applied.
#[test]
fn proposal_whose_file_changed_since_is_refused() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let mine = b.propose_on(Some(&head), &write_diff(dir.path(), "nl.diff", NIGHT_LIGHT), "Night light", Actor::User, false).unwrap();
    // A line added below the one the change touches: git still applies it, at the same place.
    let other = write_diff(dir.path(), "below.diff", "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1,2 @@\n { nightLight = false; }\n+# the person's note\n");
    let o = b.propose(&other, "a note", Actor::User, false).unwrap();
    b.apply(o.body.seq, "alice", false).unwrap();
    let moved = fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap();

    let err = format!("{:#}", b.apply(mine.body.seq, "casey", false).unwrap_err());
    assert!(err.contains("would now do something different from what was shown") && err.contains("prepare it again"), "{err}");
    assert_eq!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap(), moved, "nothing applied");
    assert_eq!(kinds(&b).iter().filter(|k| *k == "applied").count(), 1, "only the note");
}

/// ADR-053 (d): re-checked and doing what its effect says, a proposal whose tier is not the one it was
/// gated on (a ledger from elsewhere) is still refused: the re-check adds to H1's check, never replaces it.
#[test]
fn a_rechecked_proposal_is_still_held_to_its_tier_and_files() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    // The effect as the broker records it, from a real proposal of the same diff.
    let real = b.propose_on(Some(&head), &write_diff(dir.path(), "nl.diff", NIGHT_LIGHT), "Night light", Actor::User, false).unwrap();
    let said = |tier: &str, files: serde_json::Value| {
        b.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: None,
            kind: "proposal".into(),
            tier: tier.into(),
            tainted: false,
            reason: "says it is someone's own".into(),
            data: serde_json::json!({ "diff": NIGHT_LIGHT, "effect_sha256": real.body.data["effect_sha256"], "files": files, "base": head }),
        }).unwrap().body.seq
    };
    let user_tier = said("user", serde_json::json!(["desktop.nix"]));
    let other_files = said("system", serde_json::json!(["desktop.nix", "users/casey.nix"]));
    let o = b.propose(&write_diff(dir.path(), "theme.diff", THEME), "theme", Actor::User, false).unwrap();
    b.apply(o.body.seq, "alice", false).unwrap();
    for p in [user_tier, other_files] {
        let err = format!("{:#}", b.apply(p, "casey", false).unwrap_err());
        assert!(err.contains("no longer does what it was judged on"), "{err}");
    }
    assert!(fs::read_to_string(b.repo.root().join("desktop.nix")).unwrap().contains("false"), "nothing applied");
    // The real one, with what it was gated on, lands.
    assert!(b.apply(real.body.seq, "casey", false).unwrap().body.data["rechecked_on"].is_string());
}

/// ADR-053 (e): an agent's proposal, re-checked on a moved configuration, is judged by its list as the
/// list is now — narrowed since, refused; as it was, applied.
#[test]
fn an_agents_rechecked_proposal_is_judged_by_its_list_on_the_new_head() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "htop.diff", &new_user_file("users/casey/packages.json", "[\"htop\"]"));
    let p = b.propose_in(Some(&head), &d, "install htop", Actor::Agent, false, None).unwrap();
    let o = b.propose(&write_diff(dir.path(), "nl.diff", NIGHT_LIGHT), "night", Actor::User, false).unwrap();
    b.apply(o.body.seq, "casey", false).unwrap();
    // The owner narrowed the agent's list meanwhile.
    let narrowed = Broker::new(b.repo.root(), dir.path().join("state"), "true").with_agent_limits(&torq_broker::agent::Limits { users: vec![], settings: vec![] });
    let err = format!("{:#}", narrowed.apply(p.body.seq, "casey", true).unwrap_err());
    assert!(err.contains("the agent's limits no longer allow this"), "{err}");
    assert!(!b.repo.root().join("users/casey/packages.json").exists());
    let a = b.apply(p.body.seq, "casey", true).unwrap();
    assert!(a.body.data["rechecked_on"].is_string() && b.repo.root().join("users/casey/packages.json").exists());
}

#[test]
fn base_must_look_like_a_commit() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    // A ref would never equal HEAD's id, so it would be refused as stale anyway; the point of the
    // format check is to say what is actually wrong. Uppercase hex of the real HEAD is refused too.
    let upper = b.repo.head().unwrap().to_uppercase();
    for bad in ["HEAD", "main", "abc", "--help", "", upper.as_str()] {
        let err = format!("{:#}", b.propose_on(Some(bad), &d, "x", Actor::User, false).unwrap_err());
        assert!(err.contains("full commit id"), "{bad}: {err}");
    }
}

/// The broker with extra environment variables, as a caller (through sudo, say) could pass them.
fn run_cli_env(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_torq-broker"))
        .arg("--repo").arg(dir.join("config"))
        .arg("--state-dir").arg(dir.join("state"))
        .arg("--rebuild-cmd").arg("true")
        .envs(env.iter().copied())
        .args(args)
        .output()
        .unwrap()
}

/// A folder with a pre-commit hook that fails: if git ever runs it, a commit fails.
fn failing_hooks(dir: &Path) -> std::path::PathBuf {
    let hooks = dir.join("evil-hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    fs::write(&hook, "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran\"\nexit 1\n").unwrap();
    fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    hooks
}

/// Nothing in the caller's environment reaches the git (or rebuild) the broker runs: not GIT_*,
/// not git configuration in variables, not the caller's ~/.gitconfig (review N3). These run without
/// root, so the caller's HOME is kept and only GIT_CONFIG_GLOBAL keeps its .gitconfig out; as root,
/// HOME is /root as well.
#[test]
fn the_callers_environment_does_not_reach_git() {
    let (dir, b) = setup("true");
    let hooks = failing_hooks(dir.path());
    let home = dir.path().join("caller-home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join(".gitconfig"), format!("[core]\n\thooksPath = {}\n", hooks.display())).unwrap();
    let hooks_s = hooks.display().to_string();
    let home_s = home.display().to_string();
    let hostile: [&[(&str, &str)]; 3] = [
        &[("GIT_DIR", "/nonexistent")],
        &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "core.hooksPath"), ("GIT_CONFIG_VALUE_0", &hooks_s)],
        &[("HOME", &home_s)],
    ];
    for env in hostile {
        let d = write_diff(dir.path(), "theme.diff", THEME);
        let out = run_cli_env(dir.path(), env, &["--json", "propose", "--diff", &d.to_string_lossy(), "--reason", "x"]);
        assert!(out.status.success(), "{env:?}: {}", String::from_utf8_lossy(&out.stdout));
        let seq = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["seq"].as_u64().unwrap();
        let out = run_cli_env(dir.path(), env, &["--json", "apply", &seq.to_string(), "--approved-by", "casey"]);
        assert!(out.status.success(), "{env:?}: {}", String::from_utf8_lossy(&out.stdout));
        assert!(!hooks.join("ran").exists(), "{env:?}: the caller's hook ran");
        let applied = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["seq"].as_u64().unwrap();
        b.revert(applied, "casey").unwrap(); // back to the start for the next case
    }
}


/// The programs the broker runs (git, the rebuild) work from `/`, never the caller's directory: a
/// relative path in a rebuild command can never mean something the caller put where they stand.
#[test]
fn children_run_in_the_root_folder() {
    let (dir, _b) = setup("true");
    let d = write_diff(dir.path(), "theme.diff", THEME);
    let seen = dir.path().join("cwd");
    let rebuild = format!("pwd > '{}'", seen.display());
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_torq-broker"))
            .current_dir(dir.path())
            .arg("--repo").arg(dir.path().join("config"))
            .arg("--state-dir").arg(dir.path().join("state"))
            .arg("--rebuild-cmd").arg(&rebuild)
            .args(args)
            .output()
            .unwrap()
    };
    let out = run(&["--json", "propose", "--diff", &d.to_string_lossy(), "--reason", "x"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let seq = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["seq"].as_u64().unwrap();
    let out = run(&["--json", "apply", &seq.to_string(), "--approved-by", "casey"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(fs::read_to_string(&seen).unwrap(), "/\n", "the rebuild ran in /");
}
/// The broker works from `/`, whatever the caller's directory; a relative `--diff` still means the
/// file the caller meant (it is made absolute first).
#[test]
fn relative_diff_path_is_the_callers() {
    let (dir, _b) = setup("true");
    write_diff(dir.path(), "theme.diff", THEME);
    let out = Command::new(env!("CARGO_BIN_EXE_torq-broker"))
        .current_dir(dir.path())
        .args(["--repo", "config", "--state-dir", "state", "--rebuild-cmd", "true"])
        .args(["--json", "propose", "--diff", "theme.diff", "--reason", "x"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
}

/// A caller's umask does not reach what the broker writes: the config repo stays readable to the
/// unprivileged CLI even when the caller's umask is 077.
#[test]
fn callers_umask_does_not_reach_the_repo() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "theme.diff", THEME);
    let broker = format!(
        "umask 077; exec '{}' --repo '{}' --state-dir '{}' --rebuild-cmd true",
        env!("CARGO_BIN_EXE_torq-broker"),
        dir.path().join("config").display(),
        dir.path().join("state").display()
    );
    let sh = |args: &str| Command::new("sh").arg("-c").arg(format!("{broker} {args}")).output().unwrap();
    let out = sh(&format!("--json propose --diff '{}' --reason x", d.display()));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let seq = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["seq"].as_u64().unwrap();
    let out = sh(&format!("--json apply {seq} --approved-by casey"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stdout));
    let perms = fs::metadata(dir.path().join("config/users/casey.nix")).unwrap().permissions();
    let mode = std::os::unix::fs::PermissionsExt::mode(&perms) & 0o777;
    assert_eq!(mode, 0o644, "the changed file is readable to others");
    drop(b);
}

// ---- commit confirmed (ADR-016) ----

const SYS1: &str = "/nix/store/aaa-system-1";
const SYS2: &str = "/nix/store/bbb-system-2";

/// A machine whose running system and boot default are links the commands move, as the real ones
/// do: `build` prints SYS2, `activate`/`boot` point the links at `{system}`. Every command logs, in
/// order, to `log`.
struct Confirmable {
    dir: tempfile::TempDir,
    b: Broker,
    cur: std::path::PathBuf,
    profile: std::path::PathBuf,
}

const BUILD: &str = "echo build >> LOG && echo SYS2";
const ACTIVATE: &str = "ln -sfn {system} CUR && echo activate {system} >> LOG";
const SCHEDULE: &str = "echo schedule {seq} {seconds} {broker} {deadline} >> LOG";

impl Confirmable {
    fn log(&self) -> String {
        fs::read_to_string(self.dir.path().join("log")).unwrap_or_default()
    }
    /// The commands with CUR, PROFILE, LOG, SYS2 filled in: rebuild, build, activate, boot, schedule.
    fn commands(dir: &Path, build: &str, activate: &str, schedule: &str) -> [String; 5] {
        let fill = |s: &str| {
            s.replace("CUR", &dir.join("current-system").display().to_string())
                .replace("PROFILE", &dir.join("profile").display().to_string())
                .replace("LOG", &dir.join("log").display().to_string())
                .replace("SYS2", SYS2)
                .replace("DIR", &dir.display().to_string())
        };
        [
            fill("echo rebuild >> LOG"),
            fill(build),
            fill(activate),
            fill("ln -sfn {system} PROFILE && echo boot {system} >> LOG"),
            fill(schedule),
        ]
    }
    fn new_with(build: &str, activate: &str, schedule: &str) -> Self {
        let (dir, repo) = config_dir();
        let (cur, profile) = (dir.path().join("current-system"), dir.path().join("profile"));
        std::os::unix::fs::symlink(SYS1, &cur).unwrap();
        std::os::unix::fs::symlink(SYS1, &profile).unwrap();
        fs::write(dir.path().join("boot_id"), "boot-1\n").unwrap();
        let [r, bd, a, bt, s] = Self::commands(dir.path(), build, activate, schedule);
        let b = Broker::new(&repo, dir.path().join("state"), r)
            .with_current_system(&cur)
            .with_system_profile(&profile)
            .with_confirm_commands(&bd, &a, &bt, &s)
            .with_boot_id_file(dir.path().join("boot_id"))
            .with_exe("/nix/store/old-broker/bin/torq-broker");
        b.init().unwrap();
        Confirmable { dir, b, cur, profile }
    }
    fn new() -> Self {
        Self::new_with(BUILD, ACTIVATE, SCHEDULE)
    }
    fn propose_theme(&self) -> u64 {
        let d = write_diff(self.dir.path(), "theme.diff", THEME);
        self.b.propose(&d, "Light theme", Actor::User, false).unwrap().body.seq
    }
    fn link(p: &Path) -> String {
        fs::read_link(p).unwrap().display().to_string()
    }
    /// The broker binary on this machine, with `activate` (and the others) as given.
    fn cli(&self, build: &str, activate: &str, args: &[&str]) -> std::process::Output {
        let [r, bd, a, bt, s] = Self::commands(self.dir.path(), build, activate, SCHEDULE);
        Command::new(env!("CARGO_BIN_EXE_torq-broker"))
            .arg("--repo").arg(self.b.repo.root())
            .arg("--state-dir").arg(self.dir.path().join("state"))
            .arg("--current-system").arg(&self.cur)
            .arg("--system-profile").arg(&self.profile)
            .args(["--rebuild-cmd", &r, "--build-cmd", &bd, "--activate-cmd", &a, "--boot-cmd", &bt, "--schedule-cmd", &s])
            .args(args)
            .output()
            .unwrap()
    }
    /// Back as before the change: config, running system, boot default; status clean.
    fn assert_back(&self, anchor: &str) {
        assert_eq!(self.b.repo.head().unwrap(), anchor, "the config is back");
        assert_eq!((Self::link(&self.cur), Self::link(&self.profile)), (SYS1.to_string(), SYS1.to_string()));
        let s = self.b.status().unwrap();
        assert!(s.pending_confirm.is_none() && s.problems().is_empty(), "{:?}", s.problems());
    }
}

#[test]
fn commit_confirmed_then_confirm() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    let c = &a.body.data["confirm"];
    assert_eq!((c["minutes"].as_u64(), c["anchor_commit"].as_str(), c["anchor_system"].as_str()), (Some(10), Some(anchor.as_str()), Some(SYS1)));
    assert_eq!(c["boot_id"], "boot-1");
    assert_eq!(a.body.data["system"], SYS2, "the new system is running");
    assert_eq!(Confirmable::link(&m.profile), SYS1, "the boot default is untouched: a reboot rolls back");
    // Built, then the rollback scheduled, and only then activated: never the new system without a timer.
    let log = m.log();
    let sched = format!("schedule {} 600 /nix/store/old-broker/bin/torq-broker ", c["timer"]);
    let (b, s, act) = (log.find("build").unwrap(), log.find(&sched).unwrap(), log.find(&format!("activate {SYS2}")).unwrap());
    assert!(b < s && s < act, "{log}");
    assert!(log[s..].lines().next().unwrap().ends_with(" UTC"), "a wall-clock deadline: {log}");
    assert!(!log.contains("rebuild"), "{log}");
    // Waiting is not drift: the boot default is the anchor on purpose.
    let st = m.b.status().unwrap();
    assert_eq!(st.pending_confirm.as_ref().map(|pc| pc.applied), Some(a.body.seq));
    assert!(st.problems().is_empty(), "{:?}", st.problems());

    // Nothing else changes the machine meanwhile.
    let d = write_diff(m.dir.path(), "night.diff", NIGHT_LIGHT);
    let other = m.b.propose(&d, "Night light", Actor::User, false).unwrap().body.seq;
    for err in [
        m.b.apply(other, "casey", false).unwrap_err(),
        m.b.apply_confirmed(other, "casey", false, 5).unwrap_err(),
        m.b.revert(a.body.seq, "casey").unwrap_err(),
        m.b.repair("casey").unwrap_err(),
    ] {
        assert!(format!("{err:#}").contains("waiting for confirmation"), "{err:#}");
    }

    let c = m.b.confirm("casey").unwrap();
    assert_eq!((c.body.kind.as_str(), c.body.data["applied"].as_u64()), ("confirmed", Some(a.body.seq)));
    assert_eq!(Confirmable::link(&m.profile), SYS2, "confirmed: now the boot default");
    assert!(m.b.status().unwrap().problems().is_empty());
    // Its timer still fires, and finds nothing to do; later changes go ahead.
    assert!(m.b.expire_confirm(Some(a.body.seq), false).unwrap().is_none());
    assert!(format!("{:#}", m.b.confirm("casey").unwrap_err()).contains("nothing is waiting"));
    m.b.apply(other, "casey", false).unwrap();
}

#[test]
fn unconfirmed_commit_rolls_back_at_the_deadline() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 1).unwrap();
    let committed = a.body.data["commit"].as_str().unwrap().to_string();
    let r = m.b.expire_confirm(Some(a.body.seq), false).unwrap().expect("rolled back");
    assert_eq!(r.body.kind, "rolled_back");
    assert_eq!((r.body.data["unconfirmed"].as_bool(), r.body.data["by_reboot"].as_bool(), r.body.data["restored"].as_bool()), (Some(true), Some(false), Some(true)));
    assert_eq!(r.body.data["commit"], anchor.as_str());
    assert!(fs::read_to_string(m.b.repo.root().join("users/casey.nix")).unwrap().contains("dark"));
    assert!(m.log().contains(&format!("activate {SYS1}")) && !m.log().contains("rebuild"), "activated directly, not rebuilt: {}", m.log());
    // The unconfirmed commit is kept for the audit.
    assert_eq!(git(&m.b, &["rev-parse", &format!("refs/torq/discarded/{committed}")]).trim(), committed);
    m.assert_back(&anchor);
    // Once only; and too late to confirm.
    assert!(m.b.expire_confirm(None, false).unwrap().is_none());
    assert!(format!("{:#}", m.b.confirm("casey").unwrap_err()).contains("nothing is waiting"));
    m.b.ledger.verify().unwrap();
}

#[test]
fn a_reboot_before_confirming_rolls_back() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    // The boot-time unit also starts on the switch that first adds it: without a restart, it does
    // nothing (the change is waiting for its owner, not abandoned).
    assert!(m.b.expire_confirm(None, true).unwrap().is_none());
    assert!(m.b.status().unwrap().pending_confirm.is_some());
    // The reboot: a new boot ID, and the old system running (the boot default was never changed).
    fs::write(m.dir.path().join("boot_id"), "boot-2\n").unwrap();
    switch_to(&m.cur, SYS1);
    let err = m.b.confirm("casey").unwrap_err();
    assert!(format!("{err:#}").contains("restarted since"), "{err:#}");
    let r = m.b.expire_confirm(None, true).unwrap().expect("rolled back");
    assert_eq!((r.body.data["by_reboot"].as_bool(), r.body.data["restored"].as_bool()), (Some(true), Some(true)));
    assert_eq!(r.body.data["applied"].as_u64(), Some(a.body.seq));
    assert!(!m.log().contains(&format!("activate {SYS1}")), "already running the old system: {}", m.log());
    m.assert_back(&anchor);
}

/// A confirm that died after setting the boot default, then a restart into the new system: the
/// change was never confirmed, and is not confirmable now.
#[test]
fn a_restart_into_the_unconfirmed_system_is_still_rolled_back() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    switch_to(&m.profile, SYS2);
    fs::write(m.dir.path().join("boot_id"), "boot-2\n").unwrap();
    let err = m.b.confirm("casey").unwrap_err();
    assert!(format!("{err:#}").contains("restarted since"), "{err:#}");
    let r = m.b.expire_confirm(None, true).unwrap().unwrap();
    assert_eq!(r.body.data["by_reboot"].as_bool(), Some(true));
    m.assert_back(&anchor);
}

#[test]
fn a_moved_boot_default_is_put_back_by_the_rollback() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    switch_to(&m.profile, SYS2); // e.g. a confirm that died between the profile and the ledger
    m.b.expire_confirm(None, false).unwrap().unwrap();
    m.assert_back(&anchor);
}

/// switch-to-configuration exits non-zero when a unit fails to start, after switching: the restore
/// is judged by where the machine ended up.
#[test]
fn a_restore_is_judged_by_the_machine_not_the_exit_code() {
    let m = Confirmable::new_with(BUILD, "ln -sfn {system} CUR; exit 4", SCHEDULE);
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    // The apply's own activation "fails" too: it is undone, restored.
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("activation command failed"), "{err:#}");
    let last = m.b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.data["restored"].as_bool()), ("apply_failed", Some(true)));
    m.assert_back(&anchor);
}

/// A rollback that does not take is not final: it stays pending, the change unsettled, and the next
/// try (the timer's retry, any operation, the next boot) finishes it.
#[test]
fn a_rollback_that_did_not_take_is_retried() {
    // Activation fails, without switching, while DIR/broken exists.
    let m = Confirmable::new_with(BUILD, "test -e DIR/broken && exit 1; ln -sfn {system} CUR", SCHEDULE);
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    let broken = m.dir.path().join("broken");
    fs::write(&broken, "").unwrap();
    let err = m.b.expire_confirm(Some(a.body.seq), false).unwrap_err();
    assert!(format!("{err:#}").contains("did not take"), "{err:#}");
    assert!(m.b.ledger.read_all().unwrap().iter().all(|e| e.body.kind != "rolled_back"));
    let st = m.b.status().unwrap();
    assert_eq!(st.interrupted.map(|i| i.op), Some("expire".to_string()));
    assert!(st.pending_confirm.is_some(), "still unsettled: nothing else may change the machine");
    fs::remove_file(&broken).unwrap();
    let r = m.b.expire_confirm(Some(a.body.seq), false).unwrap().expect("finished");
    assert_eq!(r.body.data["restored"].as_bool(), Some(true));
    m.assert_back(&anchor);
}

#[test]
fn failed_build_changes_nothing() {
    let m = Confirmable::new_with("echo build >> LOG; exit 1", ACTIVATE, SCHEDULE);
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("build command failed"), "{err:#}");
    assert!(!m.log().contains("schedule") && !m.log().contains("activate"), "{}", m.log());
    assert_eq!(m.b.ledger.read_all().unwrap().pop().unwrap().body.kind, "apply_failed");
    m.assert_back(&anchor);
}

#[test]
fn unschedulable_rollback_is_never_activated() {
    let m = Confirmable::new_with(BUILD, ACTIVATE, "false");
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("rollback timer command failed"), "{err:#}");
    assert!(!m.log().contains("activate"), "no timer, no new system: {}", m.log());
    assert_eq!(m.b.ledger.read_all().unwrap().pop().unwrap().body.kind, "apply_failed");
    m.assert_back(&anchor);
}

#[test]
fn commit_confirmed_preconditions() {
    let m = Confirmable::new();
    let seq = m.propose_theme();
    for minutes in [0, 24 * 60 + 1] {
        let err = m.b.apply_confirmed(seq, "casey", false, minutes).unwrap_err();
        assert!(format!("{err:#}").contains("1 to 1440 minutes"), "{err:#}");
    }
    switch_to(&m.profile, "/nix/store/ccc-system-9");
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("next boot would start"), "{err:#}");
    switch_to(&m.profile, SYS1);
    fs::remove_file(m.dir.path().join("boot_id")).unwrap();
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("cannot read the boot ID"), "{err:#}");
    assert!(m.log().is_empty(), "nothing ran: {}", m.log());
}

/// A broker killed at any step of a commit confirmed leaves nothing for long: the next operation —
/// or the timer, if it was already scheduled — puts the anchor back directly, without a rebuild.
#[test]
fn crash_during_commit_confirmed_goes_back_to_the_anchor() {
    for (step, build, activate) in [
        ("build", "kill -KILL $PPID", ACTIVATE),
        // After the timer is scheduled and the new system activated, before the ledger.
        ("activation", BUILD, "ln -sfn {system} CUR; kill -KILL $PPID"),
    ] {
        let m = Confirmable::new();
        let anchor = m.b.repo.head().unwrap();
        let seq = m.propose_theme();
        assert_killed(&m.cli(build, activate, &["apply", &seq.to_string(), "--approved-by", "casey", "--confirm", "10"]));
        fs::write(m.dir.path().join("state/confirm-system"), "").unwrap(); // as nix build --out-link leaves it
        assert_eq!(m.b.status().unwrap().interrupted.map(|i| i.op), Some("apply".to_string()), "{step}");
        // What the timer runs (or any operation) reconciles first.
        assert!(m.b.expire_confirm(Some(seq + 1), false).unwrap().is_none(), "{step}: nothing was ever waiting");
        let last = m.b.ledger.read_all().unwrap().pop().unwrap();
        assert_eq!((last.body.kind.as_str(), last.body.data["restored"].as_bool()), ("interrupted", Some(true)), "{step}");
        assert!(!m.log().contains("rebuild"), "{step}: straight back, no rebuild: {}", m.log());
        m.assert_back(&anchor);
        assert!(!m.dir.path().join("state/confirm-system").exists(), "{step}: settled, so not a GC root any more");
    }
}

/// A rollback killed half way is finished — not undone — by the next broker operation.
#[test]
fn crash_mid_rollback_is_finished_by_the_next_operation() {
    let m = Confirmable::new();
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    assert_killed(&m.cli(BUILD, "kill -KILL $PPID", &["expire-confirm", &a.body.seq.to_string()]));
    assert_eq!(m.b.status().unwrap().interrupted.map(|i| i.op), Some("expire".to_string()));
    // Anything else (here: confirm) finishes the rollback first, then finds nothing to confirm.
    let err = m.b.confirm("casey").unwrap_err();
    assert!(format!("{err:#}").contains("nothing is waiting"), "{err:#}");
    let rolled: Vec<_> = m.b.ledger.read_all().unwrap().into_iter().filter(|e| e.body.kind == "rolled_back").collect();
    assert_eq!(rolled.len(), 1);
    m.assert_back(&anchor);
}

/// On a real machine the rollback uses the anchor system's own commands and PATH
/// (`<system>/etc/torq/broker.json`), not those of the change it undoes.
#[test]
fn the_rollback_uses_the_anchor_systems_own_commands() {
    let (dir, repo) = config_dir();
    let anchor_sys = dir.path().join("anchor-system");
    fs::create_dir_all(anchor_sys.join("etc/torq")).unwrap();
    let (cur, log) = (dir.path().join("current-system"), dir.path().join("log"));
    let anchor_activate = format!("ln -sfn {{system}} {} && echo anchor-activate $PATH >> {}", cur.display(), log.display());
    fs::write(
        anchor_sys.join("etc/torq/broker.json"),
        serde_json::json!({
            "repo": "/r", "stateDir": "/s", "rebuild": "x", "build": "x", "activate": anchor_activate,
            "boot": "x", "schedule": "x", "currentSystem": "/c", "systemProfile": "/p",
            "path": format!("/anchor/bin:{}", std::env::var("PATH").unwrap()), "home": "/root",
            "aFieldFromTheFuture": 1,
        })
        .to_string(),
    )
    .unwrap();
    // As a real machine has it: the boot default is the running system (a commit confirmed needs to know).
    std::os::unix::fs::symlink(&anchor_sys, dir.path().join("profile")).unwrap();
    std::os::unix::fs::symlink(&anchor_sys, &cur).unwrap();
    fs::write(dir.path().join("boot_id"), "boot-1\n").unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true")
        .with_current_system(&cur)
        .with_system_profile(dir.path().join("profile"))
        .with_boot_id_file(dir.path().join("boot_id"))
        .with_confirm_commands(
            &format!("echo {SYS2}"),
            &format!("ln -sfn {{system}} {} && echo new-activate >> {}", cur.display(), log.display()),
            "true",
            "true",
        )
        .with_anchor_settings(true);
    b.init().unwrap();
    let d = write_diff(dir.path(), "theme.diff", THEME);
    let seq = b.propose(&d, "x", Actor::User, false).unwrap().body.seq;
    let a = b.apply_confirmed(seq, "casey", false, 10).unwrap();
    b.expire_confirm(Some(a.body.seq), false).unwrap().unwrap();
    let log = fs::read_to_string(&log).unwrap();
    assert!(log.contains("anchor-activate /anchor/bin:"), "{log}");
    assert_eq!(log.matches("new-activate").count(), 1, "only the apply used this system's: {log}");
}

#[test]
fn a_pending_record_from_a_newer_broker_is_refused() {
    let m = Confirmable::new();
    fs::write(
        m.dir.path().join("state/pending.json"),
        r#"{"version":99,"op":"apply","target":2,"before":"x","ledger_len":1,"commit":null,"approved_by":"c","tier":"system","tainted":false}"#,
    )
    .unwrap();
    for err in [m.b.apply(2, "casey", false).unwrap_err(), m.b.status().unwrap_err()] {
        assert!(format!("{err:#}").contains("written by a newer broker"), "{err:#}");
    }
}

/// An activation that "succeeds" without the new system running is not a commit confirmed.
#[test]
fn activation_that_does_not_switch_is_a_failure() {
    let m = Confirmable::new_with(BUILD, "echo activate {system} >> LOG", SCHEDULE);
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let err = m.b.apply_confirmed(seq, "casey", false, 10).unwrap_err();
    assert!(format!("{err:#}").contains("the running system is not"), "{err:#}");
    m.assert_back(&anchor);
}

/// A rollback that never takes does not freeze the machine for ever: after MAX_ROLLBACK_ATTEMPTS it
/// is ledgered as not restored, the freeze lifts, and `status` says what to do.
#[test]
fn a_rollback_that_never_takes_is_settled_as_not_restored() {
    let m = Confirmable::new_with(BUILD, "test -e DIR/broken && exit 1; ln -sfn {system} CUR", SCHEDULE);
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    fs::write(m.dir.path().join("broken"), "").unwrap();
    for _ in 1..torq_broker::MAX_ROLLBACK_ATTEMPTS {
        let err = m.b.expire_confirm(Some(a.body.seq), false).unwrap_err();
        assert!(format!("{err:#}").contains("is retried"), "{err:#}");
    }
    let err = m.b.expire_confirm(Some(a.body.seq), false).unwrap_err();
    assert!(format!("{err:#}").contains("still not running"), "{err:#}");
    let last = m.b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.data["restored"].as_bool()), ("rolled_back", Some(false)));
    let st = m.b.status().unwrap();
    assert!(st.pending_confirm.is_none() && st.interrupted.is_none());
    assert!(st.problems().iter().any(|p| p.contains("could not restore")), "{:?}", st.problems());
}

/// The build is a GC root until the change settles.
#[test]
fn the_build_is_a_gc_root_until_settled() {
    let m = Confirmable::new_with("echo build {link} >> LOG && echo SYS2", ACTIVATE, SCHEDULE);
    let link = m.dir.path().join("state/confirm-system");
    let seq = m.propose_theme();
    m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    assert!(m.log().contains(&format!("build {}", link.display())), "{}", m.log());
    fs::write(&link, "").unwrap(); // what nix build --out-link would leave
    m.b.confirm("casey").unwrap();
    assert!(!link.exists(), "removed once confirmed");
}

/// `users/<name>/cli.json` is not a person's data any more (torqcli speaks one syntax, ADR-021): like
/// any file without a typed reader there, it is system tier — a person approves it.
#[test]
fn a_user_cli_style_file_is_no_longer_user_data() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "cli.diff", &new_user_file("users/casey/cli.json", r#"{ "style": "c" }"#));
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "old style file", Actor::User, false).unwrap();
    assert_eq!(p.body.tier, "system");
}

/// Only the one who made a commit confirmed confirms it (review of torqcli 3c).
#[test]
fn only_its_author_confirms_a_commit_confirmed() {
    let m = Confirmable::new();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    assert_eq!(m.b.status().unwrap().pending_confirm.unwrap().approved_by.as_deref(), Some("casey"));
    let e = format!("{:#}", m.b.confirm("dana").unwrap_err());
    assert!(e.contains(&format!("change #{} is waiting for casey to confirm it", a.body.seq)), "{e}");
    assert!(m.b.status().unwrap().pending_confirm.is_some(), "still waiting");
    m.b.confirm("casey").unwrap();
}

/// torqcli passes every value as `--name=value`: a reason that starts with `-` (any changed value
/// does, "-   host-name two; …") must reach the ledger as it is (review of torqcli 3c).
#[test]
fn a_reason_may_start_with_a_dash() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "theme.diff", THEME);
    let reason = "-   theme dark; +   theme light;";
    let out = run_cli(dir.path(), "true", &["--json", "propose", &format!("--diff={}", d.display()), &format!("--reason={reason}"), "--actor=user"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let seq = serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["seq"].as_u64().unwrap();
    assert_eq!(b.ledger.get(seq).unwrap().body.reason, reason);
    // The form torqcli used to send is what the review caught: clap reads it as an option.
    let out = run_cli(dir.path(), "true", &["propose", "--diff", &d.to_string_lossy(), "--reason", reason]);
    assert!(!out.status.success());
}

/// From the audit of 2026-10-31: with nothing pending, apply went ahead on a broken ledger, and found
/// out only at its final append — after committing and rebuilding. Now it changes nothing.
#[test]
fn a_broken_ledger_stops_apply_before_it_changes_anything() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let ledger = dir.path().join("state/ledger.jsonl");
    fs::write(&ledger, fs::read_to_string(&ledger).unwrap().replace("Config repository initialised", "edited")).unwrap();
    let calls = dir.path().join("calls.log");
    let logging = Broker::new(dir.path().join("config"), dir.path().join("state"), format!("touch {}", calls.display()));
    let head = b.repo.head().unwrap();
    let err = format!("{:#}", logging.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(err.contains("the ledger is broken") && err.contains("nothing was changed"), "{err}");
    assert_eq!(b.repo.head().unwrap(), head, "not committed");
    assert!(!calls.exists(), "not rebuilt");
    let err = format!("{:#}", logging.revert(1, "casey").unwrap_err());
    assert!(err.contains("the ledger is broken"), "{err}");
}

/// From the audit: a commit made outside the broker could become the base of an approved change, and
/// so enter the history unreviewed. Nothing builds on an unledgered commit: not a proposal (with or
/// without --base), not an apply of one proposed before, not a revert.
#[test]
fn nothing_builds_on_an_unledgered_commit() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let before = b.propose(&d, "Night light", Actor::User, false).unwrap();
    let applied = b.apply(before.body.seq, "casey", false).unwrap();
    let pending = b.propose(&write_diff(dir.path(), "t.diff", THEME), "theme", Actor::User, false).unwrap();
    fs::write(b.repo.root().join("outside.nix"), "{ }\n").unwrap();
    git(&b, &["add", "-A"]);
    git(&b, &["-c", "user.name=x", "-c", "user.email=x@x", "commit", "-qm", "outside"]);
    let outside = b.repo.head().unwrap();
    let t = write_diff(dir.path(), "t2.diff", THEME);
    for err in [
        b.propose_on(Some(&outside), &t, "on it", Actor::User, false).unwrap_err(),
        b.propose(&t, "on it", Actor::User, false).unwrap_err(),
        b.apply(pending.body.seq, "casey", false).unwrap_err(),
        b.revert(applied.body.seq, "casey").unwrap_err(),
    ] {
        let err = format!("{err:#}");
        assert!(err.contains("not the last ledgered commit") && err.contains("reset --hard"), "{err}");
    }
    assert_eq!(b.repo.head().unwrap(), outside, "nothing was added on top of it");
    // Returned to the ledgered commit, it all works again.
    git(&b, &["reset", "-q", "--hard", "HEAD~1"]);
    b.apply(pending.body.seq, "casey", false).unwrap();
}

/// From the audit: `init` again, on a repo with a ledger, committed whatever was in the working tree
/// (`git add -A`) as a new, unledgered "baseline". Now it changes nothing.
#[test]
fn init_again_changes_nothing() {
    let (_dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    fs::write(b.repo.root().join("stray.nix"), "{ }\n").unwrap();
    b.init().unwrap();
    assert_eq!(b.repo.head().unwrap(), head, "no new commit");
    assert!(git(&b, &["status", "--porcelain"]).contains("?? stray.nix"), "left as it was");
    assert_eq!(b.ledger.read_all().unwrap().len(), 1);
}

/// From the audit: an unreadable boot profile counted as "the next boot is fine". A commit confirmed
/// needs to know a reboot rolls back, so without a readable profile it is refused (a plain one works).
#[test]
fn commit_confirmed_needs_to_know_the_next_boot() {
    let (dir, b, _) = setup_with_system();
    let b = b.with_system_profile(dir.path().join("no-profile"));
    let seq = b.propose(&write_diff(dir.path(), "t.diff", THEME), "x", Actor::User, false).unwrap().body.seq;
    let err = format!("{:#}", b.apply_confirmed(seq, "casey", false, 10).unwrap_err());
    assert!(err.contains("cannot tell what the next boot starts"), "{err}");
    b.apply(seq, "casey", false).unwrap();
}

/// From the audit: a rollback whose boot default could not be read afterwards counted as
/// `restored: true`. Restored means known to be: running and next boot both the anchor.
#[test]
fn a_rollback_is_restored_only_if_the_next_boot_is_known() {
    let m = Confirmable::new();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 1).unwrap();
    // By the deadline the profile is unreadable: a folder (the boot command, `ln -sfn`, puts its link
    // inside it, so it stays one), whose target is unknown.
    fs::remove_file(&m.profile).unwrap();
    fs::create_dir(&m.profile).unwrap();
    // Not taken (and retried), never ledgered as restored.
    let err = format!("{:#}", m.b.expire_confirm(Some(a.body.seq), false).unwrap_err());
    assert!(err.contains("did not take") && err.contains("next boot ?"), "{err}");
    assert!(!m.b.ledger.read_all().unwrap().iter().any(|e| e.body.kind == "rolled_back"), "not ledgered as done");
}

/// From the audit: apply and revert did not check for an outside switch running (repair and
/// reconcile did); one interleaving could have its system recorded as the broker's commit's.
/// `unit` is the switch unit a systemctl of the test's says is active.
fn apply_and_revert_wait_for_a_switch_in(unit: &str) {
    let (dir, b) = setup("true");
    let fake = dir.path().join("fake-bin");
    fs::create_dir(&fake).unwrap();
    // A systemctl that says the switch unit is active.
    fs::write(fake.join("systemctl"), format!("#!/bin/sh\n[ \"$1 $3\" = \"is-active {unit}\" ]\n")).unwrap();
    fs::set_permissions(fake.join("systemctl"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", fake.display(), std::env::var("PATH").unwrap());
    let d = write_diff(dir.path(), "theme.diff", THEME);
    let p = b.propose(&d, "x", Actor::User, false).unwrap();
    let head = b.repo.head().unwrap();
    let out = run_cli_env(dir.path(), &[("PATH", &path)], &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("running outside the broker"), "{unit}: {out:?}");
    assert_eq!(b.repo.head().unwrap(), head, "nothing committed");
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let out = run_cli_env(dir.path(), &[("PATH", &path)], &["revert", &a.body.seq.to_string(), "--approved-by", "casey"]);
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("running outside the broker"), "{unit}: {out:?}");
}

/// A switch by hand with nixos-rebuild (a stock NixOS habit; the tool is not on a TorqOS machine,
/// but can be brought).
#[test]
fn apply_and_revert_wait_for_an_outside_switch() {
    apply_and_revert_wait_for_a_switch_in("nixos-rebuild-switch-to-configuration.service");
}

/// The broker's own switch unit (ADR-032), left running by a broker that died, or run by hand.
#[test]
fn apply_and_revert_wait_for_the_brokers_own_switch_unit() {
    apply_and_revert_wait_for_a_switch_in("torq-switch.service");
}

/// From the audit: a commit git had made when what followed it failed (its sync, here) was reset
/// away without being kept for the audit. It is kept, as reconcile keeps one.
#[test]
fn a_commit_undone_after_a_failure_is_kept() {
    let (dir, b) = setup("true");
    let fake = dir.path().join("fake-bin");
    fs::create_dir(&fake).unwrap();
    fs::write(fake.join("sync"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(fake.join("sync"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", fake.display(), std::env::var("PATH").unwrap());
    let p = b.propose(&write_diff(dir.path(), "theme.diff", THEME), "x", Actor::User, false).unwrap();
    let before = b.repo.head().unwrap();
    let out = run_cli_env(dir.path(), &[("PATH", &path)], &["apply", &p.body.seq.to_string(), "--approved-by", "casey"]);
    assert!(!out.status.success(), "{out:?}");
    let kept = git(&b, &["for-each-ref", "--format=%(objectname)", "refs/torq/discarded/"]);
    let kept: Vec<&str> = kept.lines().collect();
    assert_eq!(kept.len(), 1, "the commit made is kept: {kept:?}");
    assert_ne!(kept[0], before);
    assert!(git(&b, &["show", &format!("{}:users/casey.nix", kept[0])]).contains("light"), "it is the change's");
}

/// From the second audit: the broker's git ran the repo's own hooks (`.git/hooks/*`), as root. No
/// hook in the repo runs, whatever the operation.
#[test]
fn the_repos_own_hooks_never_run() {
    let (dir, b) = setup("true");
    let ran = dir.path().join("hook-ran");
    for hook in ["pre-commit", "commit-msg", "post-commit", "pre-applypatch", "post-checkout", "reference-transaction"] {
        let h = b.repo.root().join(".git/hooks").join(hook);
        fs::write(&h, format!("#!/bin/sh\necho {hook} >> '{}'\n", ran.display())).unwrap();
        fs::set_permissions(&h, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    let p = b.propose(&write_diff(dir.path(), "theme.diff", THEME), "x", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    b.revert(a.body.seq, "casey").unwrap();
    assert!(!ran.exists(), "a hook ran: {}", fs::read_to_string(&ran).unwrap_or_default());
}

/// From the second audit: an agent's proposal must say its base (M2), and no proposal may be huge (M4).
#[test]
fn an_agent_says_its_base_and_no_proposal_is_huge() {
    let (dir, b) = setup("true");
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let err = format!("{:#}", b.propose(&d, "x", Actor::Agent, false).unwrap_err());
    assert!(err.contains("needs --base"), "{err}");
    let pkgs = write_diff(dir.path(), "p.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    b.propose_on(Some(&b.repo.head().unwrap()), &pkgs, "x", Actor::Agent, false).unwrap();
    b.propose(&d, "a person's needs none", Actor::User, false).unwrap();
    let big = dir.path().join("big.diff");
    fs::File::create(&big).unwrap().set_len(torq_broker::MAX_DIFF + 1).unwrap();
    let err = format!("{:#}", b.propose(&big, "x", Actor::User, false).unwrap_err());
    assert!(err.contains("at most"), "{err}");
}

/// The second audit (H4): the rebuild built the config repo's working tree, so "the running system is
/// the ledgered commit" held only because nothing else writes there. `{flake}` is the repo at the
/// commit: exactly the one just made, or, after a failed rebuild, the one restored to.
#[test]
fn the_rebuild_builds_exactly_the_commit() {
    let (dir, _b) = setup("true");
    let seen = dir.path().join("flake");
    let b = Broker::new(dir.path().join("config"), dir.path().join("state"), format!("echo {{flake}} >> '{}'", seen.display()));
    let p = b.propose(&write_diff(dir.path(), "theme.diff", THEME), "x", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let commit = a.body.data["commit"].as_str().unwrap().to_string();
    let root = b.repo.root().display().to_string();
    assert_eq!(fs::read_to_string(&seen).unwrap(), format!("git+file://{root}?rev={commit}\n"));
    // A failed rebuild: the restore builds the commit before, by its id too.
    let before = b.repo.head().unwrap();
    let failing = Broker::new(dir.path().join("config"), dir.path().join("state"), format!("echo {{flake}} >> '{}'; [ \"$(wc -l < '{}')\" -gt 2 ]", seen.display(), seen.display()));
    let p = failing.propose(&write_diff(dir.path(), "nl.diff", NIGHT_LIGHT), "x", Actor::User, false).unwrap();
    assert!(failing.apply(p.body.seq, "casey", false).is_err());
    let lines: Vec<String> = fs::read_to_string(&seen).unwrap().lines().map(String::from).collect();
    assert_eq!(lines.last().unwrap(), &format!("git+file://{root}?rev={before}"), "{lines:?}");
}

/// A commit confirmed's build too: `{flake}` is the config repo at the commit it builds.
#[test]
fn the_confirmed_build_builds_exactly_the_commit() {
    let m = Confirmable::new_with("echo flake {flake} >> LOG && echo SYS2", ACTIVATE, SCHEDULE);
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    let commit = a.body.data["commit"].as_str().unwrap();
    assert!(m.log().contains(&format!("flake git+file://{}?rev={commit}", m.b.repo.root().display())), "{}", m.log());
}

/// `{flake}` is a URL: built only from an absolute path of plain characters (and a full commit id).
#[test]
fn the_flake_needs_a_plain_absolute_repo_path() {
    let base = tempfile::tempdir().unwrap();
    let odd = base.path().join("con fig");
    let (dir, repo) = config_dir();
    fs::rename(&repo, &odd).unwrap();
    let b = Broker::new(&odd, dir.path().join("state"), "echo {flake}");
    b.init().unwrap();
    let p = b.propose(&write_diff(dir.path(), "theme.diff", THEME), "x", Actor::User, false).unwrap();
    let err = format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(err.contains("not an absolute path of plain characters"), "{err}");
}

/// First-boot setup (docs/INSTALL.md): the repo just written is taken over, built and switched to —
/// and only then ledgered, as change #1 with the system it really switched to (not the image's).
/// Once only; a failed build ledgers nothing and leaves the files for setup to try again.
#[test]
fn setup_builds_switches_then_ledgers_once() {
    let (dir, repo) = config_dir();
    let link = dir.path().join("current-system");
    std::os::unix::fs::symlink("/nix/store/aaa-the-image", &link).unwrap();
    // The "switch": the running system becomes the configuration's.
    let switch = format!("ln -sfn /nix/store/bbb-the-machine {}", link.display());
    let b = Broker::new(&repo, dir.path().join("state"), &switch)
        .with_current_system(&link)
        .with_system_profile(dir.path().join("no-profile"))
        .with_confirm_commands("echo /nix/store/bbb-the-machine", "true", "true", "true");
    let e = b.set_up().unwrap();
    assert_eq!((e.body.seq, e.body.kind.as_str(), e.body.reason.as_str()), (1, "init", "TorqOS set up"));
    assert_eq!(e.body.data["system"], "/nix/store/bbb-the-machine", "the system it switched to, not the image's");
    assert_eq!(e.body.data["commit"].as_str(), Some(b.repo.head().unwrap().as_str()));
    assert!(b.status().unwrap().problems().is_empty(), "as its history says, at once");
    let again = format!("{:#}", b.set_up().unwrap_err());
    assert!(again.contains("already set up"), "{again}");
    // A repo with a history of its own (a ledger lost, say) is never taken over, nor its history removed.
    let (dir, repo) = config_dir();
    let kept = Broker::new(&repo, dir.path().join("state"), "true").with_confirm_commands("false", "true", "true", "true");
    kept.repo.init().unwrap();
    let e = format!("{:#}", kept.set_up().unwrap_err());
    assert!(e.contains("already has a history") && repo.join(".git").exists(), "{e}");

    // A configuration that does not build: nothing switched, nothing ledgered, not taken over, the
    // files kept.
    let (dir, repo) = config_dir();
    let marker = dir.path().join("switched");
    let failing = Broker::new(&repo, dir.path().join("state"), &format!("touch {}", marker.display())).with_confirm_commands("false", "true", "true", "true");
    let e = format!("{:#}", failing.set_up().unwrap_err());
    assert!(e.contains("did not build: nothing was set up"), "{e}");
    assert!(failing.ledger.read_all().unwrap().is_empty() && !marker.exists(), "never switched");
    assert!(!repo.join(".git").exists() && repo.join("desktop.nix").exists(), "files left, history gone");
    // Built, but the switch reported a problem after making it the boot default: the machine starts
    // it from now on, so it is ledgered, saying so.
    let (dir, repo) = config_dir();
    let profile = dir.path().join("system");
    let partly = Broker::new(&repo, dir.path().join("state"), &format!("ln -sfn /nix/store/ccc {} && false", profile.display()))
        .with_current_system(dir.path().join("current-system"))
        .with_system_profile(&profile)
        .with_confirm_commands("echo /nix/store/ccc", "true", "true", "true");
    let e = partly.set_up().unwrap();
    assert_eq!((e.body.seq, e.body.reason.as_str()), (1, "TorqOS set up (the switch reported a problem: show system alarms)"));
    // The switch failed before the new system took either way (neither running nor the boot
    // default): the machine is still the image, with no account — nothing is set up, as for a build
    // that failed, so setup asks again (audit A4; it was ledgered as change #1).
    let (dir, repo) = config_dir();
    let none = Broker::new(&repo, dir.path().join("state"), "false")
        .with_current_system(dir.path().join("current-system"))
        .with_system_profile(dir.path().join("no-profile"))
        .with_confirm_commands("echo /nix/store/ddd", "true", "true", "true");
    let e = format!("{:#}", none.set_up().unwrap_err());
    assert!(e.contains("still runs and starts the image: nothing was set up"), "{e}");
    assert!(none.ledger.read_all().unwrap().is_empty(), "no change #1");
    assert!(!repo.join(".git").exists() && repo.join("desktop.nix").exists(), "files left, history gone: setup can try again");
}

/// `request system password`: the hash set at once (here a command that only reads it), kept in
/// the account's file outside the repo, private; the ledger says whose and by whom — never the hash.
#[test]
fn a_password_is_kept_outside_the_repo_and_ledgered_without_it() {
    let (dir, b) = setup("true");
    let pw = dir.path().join("passwords");
    let people = ["casey".to_string()];
    let b = b.with_passwords(&pw, "cat").with_people(&people);
    let hash = "$6$saltsalt$abcdefghijklmnopqrstuvwxyz0123456789";
    let e = b.set_password("casey", hash, "casey").unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.reason.as_str(), e.body.data["user"].as_str()), ("password", "Password changed for casey", Some("casey")));
    assert_eq!(fs::read_to_string(pw.join("casey")).unwrap(), format!("{hash}\n"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(pw.join("casey")).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(&pw).unwrap().permissions().mode() & 0o777, 0o700);
    let ledger = fs::read_to_string(dir.path().join("state/ledger.jsonl")).unwrap();
    assert!(!ledger.contains("$6$"), "never the hash");
    assert!(!b.repo.root().join("casey").exists() && b.repo.ensure_clean().is_ok(), "nothing in the repo");
    for (user, bad) in [("casey", "plaintext"), ("casey", "$6$a:b"), ("casey", "$6$a\nroot:$6$x"), ("../x", hash), ("Casey", hash), ("root", hash), ("sshd", hash), ("nixbld1", hash), ("nobody", hash)] {
        assert!(b.set_password(user, bad, "casey").is_err(), "{user} {bad:?}");
    }
    // Not set in the running system (no such account): nothing kept, and the ledger says it failed.
    let failing = Broker::new(b.repo.root(), dir.path().join("state"), "true").with_passwords(&pw, "false").with_people(&people);
    let before = b.ledger.read_all().unwrap().len();
    assert!(failing.set_password("casey", "$6$other$hash", "casey").is_err());
    assert_eq!(fs::read_to_string(pw.join("casey")).unwrap(), format!("{hash}\n"), "kept as it was");
    // On the record first, so a change is never silent: the attempt, then that it failed.
    let after = b.ledger.read_all().unwrap();
    assert_eq!(after.len(), before + 2);
    assert_eq!((after[before].body.kind.as_str(), after[before + 1].body.kind.as_str()), ("password", "password_failed"));
}

/// ADR-041: what the agent may propose is a default-deny list, judged by what the change does, and
/// a refusal is on the record (kind `refused`: the files, why, the diff's hash — not the diff).
#[test]
fn the_agent_proposes_only_what_its_list_allows_and_a_refusal_is_ledgered() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    // A file that is not a setting (desktop.nix), and another person's packages: refused, ledgered.
    for (name, body, why) in [
        ("nl.diff", NIGHT_LIGHT, "not a setting"),
        ("sam.diff", "--- /dev/null\n+++ b/users/sam/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n", "packages only for casey"),
        ("svc.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}}\n", "may not change services.openssh.enable"),
        ("torq.diff", "--- /dev/null\n+++ b/nixos/torq.json\n@@ -0,0 +1 @@\n+{\"agent\": {}}\n", "may not change torq.agent"),
    ] {
        let d = write_diff(dir.path(), name, body);
        let err = format!("{:#}", b.propose_on(Some(&head), &d, "try", Actor::Agent, false).unwrap_err());
        assert!(err.contains(why), "{name}: {err}");
        let last = b.ledger.read_all().unwrap().pop().unwrap();
        assert_eq!((last.body.kind.as_str(), last.body.actor.as_str()), ("refused", "agent"), "{name}");
        assert!(last.body.data["why"].as_str().unwrap().contains(why) && last.body.data.get("diff").is_none(), "{name}: {:?}", last.body.data);
    }
    // A person may propose the same: the list is the agent's alone.
    let d = write_diff(dir.path(), "svc2.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}}\n");
    assert_eq!(b.propose_on(Some(&head), &d, "a person's", Actor::User, false).unwrap().body.kind, "proposal");
    // Listed by the owner: services.openssh, and nothing beside it.
    let (dir, b) = setup("true");
    let b = b.with_agent_limits(&torq_broker::agent::Limits { users: vec![], settings: vec!["services.openssh".into()] });
    let head = b.repo.head().unwrap();
    let ok = write_diff(dir.path(), "ok.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}}\n");
    assert_eq!(b.propose_on(Some(&head), &ok, "ssh", Actor::Agent, false).unwrap().body.kind, "proposal");
    let both = write_diff(dir.path(), "both.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}, \"nginx\": {\"enable\": true}}\n");
    let err = format!("{:#}", b.propose_on(Some(&head), &both, "ssh and more", Actor::Agent, false).unwrap_err());
    assert!(err.contains("services.nginx.enable"), "{err}");
    b.ledger.verify().unwrap();
}

/// The audit of ADR-041, its 1: a diff's text can say one thing while `git apply` does another
/// (lines past a hunk's count are ignored). What is recorded to show a person is what git does.
#[test]
fn what_a_person_is_shown_is_what_git_does() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let smuggled = write_diff(dir.path(), "s.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"nmap\"]\n-[\"nmap\"]\n+[\"htop\"]\n");
    let p = b.propose_on(Some(&head), &smuggled, "htop for casey", Actor::Agent, false).unwrap();
    let effect = &b.proposal_text(&p, "effect").unwrap();
    assert!(effect.contains("+[\"nmap\"]") && !effect.contains("htop"), "{effect}");
    b.apply(p.body.seq, "casey", true).unwrap(); // (no session: tainted)
    assert_eq!(fs::read_to_string(b.repo.root().join("users/casey/packages.json")).unwrap().trim(), "[\"nmap\"]", "what was shown is what landed");
}

/// Its 2 and 3: every refusal of the agent's is on the record (the forbidden ones, a diff that does
/// not apply), within limits that keep it from filling the ledger.
#[test]
fn every_refusal_of_the_agents_is_ledgered_within_limits() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let count = |b: &Broker| b.ledger.read_all().unwrap().iter().filter(|e| e.body.kind == "refused").count();
    let policy = write_diff(dir.path(), "p.diff", "--- /dev/null\n+++ b/policy/new.toml\n@@ -0,0 +1 @@\n+x = 1\n");
    let e = format!("{:#}", b.propose_on(Some(&head), &policy, "loosen", Actor::Agent, false).unwrap_err());
    assert!(e.contains("forbidden"), "{e}");
    assert_eq!(count(&b), 1, "a forbidden attempt is on the record");
    let broken = write_diff(dir.path(), "b.diff", "--- a/nothing.json\n+++ b/nothing.json\n@@ -1 +1 @@\n-x\n+y\n");
    assert!(b.propose_on(Some(&head), &broken, "nothing", Actor::Agent, false).is_err());
    assert_eq!(count(&b), 2, "so is a diff that does not apply");
    // A person's refusal is not the agent's record.
    assert!(b.propose_on(Some(&head), &broken, "nothing", Actor::User, false).is_err());
    assert_eq!(count(&b), 2);
    // A long reason: refused (and ledgered, shortened).
    let ok = write_diff(dir.path(), "ok.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    let long = "x".repeat(torq_broker::AGENT_REASON_MAX + 1);
    assert!(format!("{:#}", b.propose_on(Some(&head), &ok, &long, Actor::Agent, false).unwrap_err()).contains("at most"));
    let last = b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!(last.body.reason.chars().count(), torq_broker::AGENT_REASON_MAX);
    // At most so many waiting at once.
    for i in 0..torq_broker::AGENT_WAITING_MAX {
        b.propose_on(Some(&head), &ok, &format!("htop {i}"), Actor::Agent, false).unwrap();
    }
    assert!(format!("{:#}", b.propose_on(Some(&head), &ok, "one more", Actor::Agent, false).unwrap_err()).contains("waiting"));
    // And refusals ledgered at most so many an hour: the rest turned away without an entry each.
    for _ in 0..torq_broker::AGENT_REFUSALS_AN_HOUR + 5 {
        let _ = b.propose_on(Some(&head), &policy, "again", Actor::Agent, false);
    }
    assert_eq!(count(&b), torq_broker::AGENT_REFUSALS_AN_HOUR);
    b.ledger.verify().unwrap();
}

/// Its 6: the agent's list as it is when a person applies, not as it was when it proposed.
#[test]
fn the_agents_list_is_checked_again_at_apply() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let ok = write_diff(dir.path(), "ok.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    let p = b.propose_on(Some(&head), &ok, "htop", Actor::Agent, false).unwrap();
    let narrowed = b.with_agent_limits(&torq_broker::agent::Limits::default());
    let err = format!("{:#}", narrowed.apply(p.body.seq, "casey", true).unwrap_err());
    assert!(err.contains("no longer allow"), "{err}");
}

/// The second review of ADR-041: a refusal before the ledger has begun is not entry #1 (setup
/// would take it for a machine already set up); and the agent's waiting proposals are only those
/// that can still land — once a person's change means they cannot, they no longer count.
#[test]
fn the_agents_record_never_blocks_setup_or_the_agent() {
    let (dir, repo) = config_dir();
    let fresh = Broker::new(&repo, dir.path().join("state"), "true");
    let d = write_diff(dir.path(), "p.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    assert!(fresh.propose_on(Some(&"a".repeat(40)), &d, "before setup", Actor::Agent, false).is_err());
    assert!(fresh.ledger.read_all().unwrap().is_empty(), "no entry before init");
    fresh.init().unwrap();
    assert_eq!(fresh.ledger.read_all().unwrap()[0].body.kind, "init");

    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    for i in 0..torq_broker::AGENT_WAITING_MAX {
        b.propose_on(Some(&head), &d, &format!("htop {i}"), Actor::Agent, false).unwrap();
    }
    assert!(b.propose_on(Some(&head), &d, "one more", Actor::Agent, false).is_err());
    // A person writes the same file: the twenty would now do something else, so they can no longer
    // land (ADR-053's re-check), nor count.
    let mine = write_diff(dir.path(), "m.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"vim\"]\n");
    let p = b.propose(&mine, "mine", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let now = write_diff(dir.path(), "n.diff", "--- a/users/casey/packages.json\n+++ b/users/casey/packages.json\n@@ -1 +1 @@\n-[\"vim\"]\n+[\"vim\", \"htop\"]\n");
    b.propose_on(Some(&b.repo.head().unwrap()), &now, "htop, now", Actor::Agent, false).unwrap();
}

/// The review of the desktop line (2026-11-10): since ADR-053 an agent's proposal prepared on an older
/// head still lands if it does the same there, so it still counts. Another file moving the head (the
/// desktop's own save, every quarter of an hour) must not empty the count, or the agent could leave
/// twenty more waiting each time — the burying ADR-041's limit is for.
#[test]
fn the_agents_waiting_proposals_still_count_when_another_file_moves_the_head() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "p.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    for i in 0..torq_broker::AGENT_WAITING_MAX {
        b.propose_on(Some(&head), &d, &format!("htop {i}"), Actor::Agent, false).unwrap();
    }
    let mine = write_diff(dir.path(), "m.diff", NIGHT_LIGHT);
    let p = b.propose(&mine, "mine", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    let moved = b.repo.head().unwrap();
    assert_ne!(moved, head);
    let e = format!("{:#}", b.propose_on(Some(&moved), &d, "twenty-first", Actor::Agent, false).unwrap_err());
    assert!(e.contains("waiting for a person already"), "{e}");
    // They still land: the count was right to hold them.
    let first = b.ledger.read_all().unwrap().into_iter().find(|e| e.body.kind == "proposal" && e.body.actor == "agent").unwrap();
    b.apply(first.body.seq, "casey", true).unwrap();
}

/// The review of that fix: the count judges as apply judges, so what apply would refuse frees a slot.
/// The owner narrowing the agent's list, the one way a person has to turn the agent's proposals down,
/// frees them all; a proposal whose diff is no longer kept here frees its own.
#[test]
fn the_agents_waiting_proposals_that_apply_would_refuse_do_not_count() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "p.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"htop\"]\n");
    for i in 0..torq_broker::AGENT_WAITING_MAX {
        b.propose_on(Some(&head), &d, &format!("htop {i}"), Actor::Agent, false).unwrap();
    }
    assert!(b.propose_on(Some(&head), &d, "one more", Actor::Agent, false).is_err());
    // One proposal's diff gone from the state directory: it cannot be applied, so one more may wait.
    let first = b.ledger.read_all().unwrap().into_iter().find(|e| e.body.kind == "proposal" && e.body.actor == "agent").unwrap();
    fs::remove_file(dir.path().join("state/proposals").join(first.body.data["diff_sha256"].as_str().unwrap())).ok();
    let still = b.ledger.read_all().unwrap().into_iter().filter(|e| e.body.kind == "proposal" && e.body.actor == "agent").count();
    // All twenty share one diff (the same text, kept once by its hash): gone for every one of them.
    assert_eq!(still, torq_broker::AGENT_WAITING_MAX);
    b.propose_on(Some(&head), &write_diff(dir.path(), "q.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"vim\"]\n"), "vim", Actor::Agent, false).unwrap();

    // The owner narrows the list: none of the earlier ones could be applied now, nor count.
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    for i in 0..torq_broker::AGENT_WAITING_MAX {
        b.propose_on(Some(&head), &d, &format!("htop {i}"), Actor::Agent, false).unwrap();
    }
    let b = b.with_agent_limits(&torq_broker::agent::Limits { users: vec![], settings: vec!["services.openssh".into()] });
    let ssh = write_diff(dir.path(), "ssh.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}}\n");
    b.propose_on(Some(&head), &ssh, "ssh", Actor::Agent, false).unwrap();
}

/// ADR-036, the review of adopt: what a repository must be for a new machine to adopt it. A staged
/// repository (as `stage_from` leaves one) built here from a real broker's repo and ledger.
#[test]
fn a_hostile_repository_is_refused_before_anything_is_adopted() {
    let sh = |dir: &Path, script: &str| {
        let ok = std::process::Command::new("sh").arg("-c").arg(script).current_dir(dir).status().unwrap().success();
        assert!(ok, "{script}");
    };
    // A machine as setup leaves one.
    let (dir, repo) = config_dir();
    sh(&repo, "mkdir -p nixos system && echo '{\"user\": \"alice\"}' > nixos/torq.json && echo '{\"host-name\": \"first\"}' > system/host.json \
        && echo '{\"\": \"hardware-configuration.nix\"}' > config-files.json && echo '{ }' > hardware-configuration.nix && rm -rf policy");
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    b.init().unwrap();
    let new_machine = Broker::new(dir.path().join("new-config"), dir.path().join("new-state"), "true");
    fs::create_dir_all(dir.path().join("new-state")).unwrap();
    let url = "git@host:x.git";
    // Its repository as staged: main (`git_step` on a working copy) and the ledger, as pushed.
    let stage_with = |name: &str, git_step: &str, ledger: &Path| -> Repo {
        let work = dir.path().join(format!("{name}-work"));
        let stage = dir.path().join(format!("{name}.git"));
        sh(dir.path(), &format!("git clone -q {} {}", repo.display(), work.display()));
        if !git_step.is_empty() {
            sh(&work, &format!("{git_step} && git add -Af . && git -c user.name=t -c user.email=t@t commit -qm step"));
        }
        sh(dir.path(), &format!("git clone -q --bare {} {}", work.display(), stage.display()));
        sh(&stage, &format!("b=$(git hash-object -w {}) && t=$(printf '100644 blob %s\\tledger.jsonl\\n' $b | git mktree) && c=$(git -c user.name=t -c user.email=t@t commit-tree $t -m l) && git update-ref refs/torq/remote-ledger $c && git symbolic-ref HEAD refs/heads/main", ledger.display()));
        Repo::new(stage)
    };
    let ledger = b.ledger.path().to_path_buf();
    let a = new_machine.check_stage(&stage_with("good", "", &ledger), url).unwrap();
    assert_eq!((a.user.as_str(), a.host.as_str()), ("alice", "first"));
    // What is adopted is what was shown: the commits and the owner, or nothing (no second fetch).
    sh(dir.path(), &format!("cp -r good.git {}", dir.path().join("new-state/adopt.git").display()));
    let own = dir.path().join("own");
    let e = format!("{:#}", new_machine.adopt(url, &own, &"0".repeat(40), &a.tip, &a.user).unwrap_err());
    assert!(e.contains("not what was shown"), "{e}");
    let e = format!("{:#}", new_machine.adopt(url, &own, &a.head, &a.tip, "mallory").unwrap_err());
    assert!(e.contains("not what was shown"), "{e}");
    assert!(!new_machine.repo.is_initialised(), "nothing adopted");
    for (name, step, why) in [
        ("link", "rm hardware-configuration.nix && ln -s /etc/shadow hardware-configuration.nix", "not a plain file"),
        ("lock", "ln -s ../state/remote/key flake.lock", "not a plain file"),
        ("attrs", "echo '* -diff' > .gitattributes", "may not"),
        ("policy", "mkdir policy && echo x > policy/tiers.toml", "may not"),
        // A link anywhere (Nix reads through it, this check would not): a setting hidden from the console.
        ("linked", "ln -s other.json nixos/services.json && echo '{\"openssh\":{\"enable\":true}}' > nixos/other.json", "not a plain file"),
        // What cannot be read is refused, never reported as off.
        ("huge", "head -c 2000000 /dev/zero | tr '\\0' ' ' > nixos/services.json", "larger than a setting"),
        ("broken", "echo '{ not json' > nixos/services.json", "not JSON this machine can read"),
        ("noread", "echo '{}' > config-files.json", "does not read a hardware file"),
        ("moved", "echo x > stray.txt", "is not the commit its ledger ends at"),
    ] {
        let e = format!("{:#}", new_machine.check_stage(&stage_with(name, step, &ledger), url).unwrap_err());
        assert!(e.contains(why), "{name}: {e}");
    }
    // Every way the tree turns SSH on is reported: the option, its alias, the old list. (A step leaves
    // main ahead of the ledger, so each ledger here is one that ends at that commit.)
    for (name, step) in [
        ("sshd", "echo '{\"sshd\":{\"enable\":true}}' > nixos/services.json"),
        ("openssh", "echo '{\"openssh\":{\"enable\":true}}' > nixos/services.json"),
        ("oldlist", "echo '[\"ssh\"]' > services.json"),
    ] {
        let work = dir.path().join(format!("ssh-{name}"));
        sh(dir.path(), &format!("cp -r {} {}", repo.display(), work.display()));
        sh(&work, &format!("{step} && git add -A && git -c user.name=t -c user.email=t@t commit -qm step"));
        let head = String::from_utf8(Command::new("git").arg("-C").arg(&work).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
        let l = dir.path().join(format!("{name}.jsonl"));
        fs::copy(&ledger, &l).unwrap();
        torq_broker::ledger::Ledger::open(&l).append(torq_broker::ledger::NewEntry {
            actor: "user".into(), approved_by: Some("alice".into()), kind: "applied".into(), tier: "system".into(), tainted: false, reason: "ssh".into(),
            data: serde_json::json!({ "commit": head }),
        }).unwrap();
        let stage = dir.path().join(format!("ssh-{name}.git"));
        sh(dir.path(), &format!("git clone -q --bare {} {}", work.display(), stage.display()));
        sh(&stage, &format!("b=$(git hash-object -w {}) && t=$(printf '100644 blob %s\\tledger.jsonl\\n' $b | git mktree) && c=$(git -c user.name=t -c user.email=t@t commit-tree $t -m l) && git update-ref refs/torq/remote-ledger $c", l.display()));
        assert!(new_machine.check_stage(&Repo::new(stage), url).unwrap().ssh, "{name}");
    }
    // A ledger whose entry names an option for a commit, a chain well made: refused before it is trusted.
    let optioned = dir.path().join("optioned.jsonl");
    fs::copy(&ledger, &optioned).unwrap();
    torq_broker::ledger::Ledger::open(&optioned).append(torq_broker::ledger::NewEntry { actor: "user".into(), approved_by: None, kind: "proposal".into(), tier: "user".into(), tainted: false, reason: "x".into(), data: serde_json::json!({ "commit": "--output=/root/.ssh/authorized_keys" }) }).unwrap();
    let e = format!("{:#}", new_machine.check_stage(&stage_with("optioned", "", &optioned), url).unwrap_err());
    assert!(e.contains("is not a commit"), "{e}");
    // A chain that does not verify: an entry's reason changed, and its hash with it.
    let broken = dir.path().join("broken.jsonl");
    fs::write(&broken, fs::read_to_string(&ledger).unwrap().replacen("\"init\"", "\"init\"", 1).replacen("Config repository initialised", "Config repository INITIALISED", 1)).unwrap();
    let e = format!("{:#}", new_machine.check_stage(&stage_with("unchained", "", &broken), url).unwrap_err());
    assert!(e.contains("does not verify"), "{e}");
    // A ledger that ends in the middle of a line: less than the remote holds, or something else.
    let cutoff = dir.path().join("cutoff.jsonl");
    let whole = fs::read(&ledger).unwrap();
    fs::write(&cutoff, [&whole[..whole.len() - 1], b"x"].concat()).unwrap();
    let e = format!("{:#}", new_machine.check_stage(&stage_with("cutoff", "", &cutoff), url).unwrap_err());
    assert!(e.contains("middle of a line"), "{e}");
    // A ledger with a commit confirmed waiting would drive the boot's rollback: refused.
    let waiting = dir.path().join("waiting.jsonl");
    fs::copy(&ledger, &waiting).unwrap();
    let forged = torq_broker::ledger::Ledger::open(&waiting);
    forged.append(torq_broker::ledger::NewEntry {
        actor: "user".into(), approved_by: Some("alice".into()), kind: "applied".into(), tier: "system".into(), tainted: false, reason: "x".into(),
        data: serde_json::json!({ "commit": b.repo.head().unwrap(), "confirm": { "deadline": "2099-01-01T00:00:00Z", "anchor_commit": "a".repeat(40), "anchor_system": "/nix/store/x-system", "boot_id": "b" } }),
    }).unwrap();
    let e = format!("{:#}", new_machine.check_stage(&stage_with("waiting", "", &waiting), url).unwrap_err());
    assert!(e.contains("still waiting"), "{e}");
}

/// ADR-035: a stuck operation holds the broker's lock for as long as it is stuck, and that is when a
/// person needs the shell. Break-glass is recorded through it, and what it needs of its text.
#[test]
fn break_glass_is_recorded_while_an_operation_holds_the_lock() {
    let (dir, repo) = config_dir();
    let state = dir.path().join("state");
    let b = Broker::new(&repo, &state, "true");
    b.init().unwrap();
    let held = fs::File::create(state.join("broker.lock")).unwrap();
    held.try_lock().unwrap();
    let (e, unsent) = b.break_glass("alice", "the switch is stuck").unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.approved_by.as_deref()), ("break_glass", Some("alice")));
    assert_eq!(unsent, None, "no remote: nothing to send");
    drop(held);
    b.ledger.verify().unwrap();
    for (by, why) in [("alice", ""), ("alice", "  "), ("alice", "a\nb"), ("../x", "why")] {
        assert!(b.break_glass(by, why).is_err(), "{by:?} {why:?}");
    }
    assert_eq!(b.ledger.read_all().unwrap().len(), e.body.seq as usize, "refused: nothing written");
}

/// Writers of the ledger do not interleave: appends from many threads leave one unbroken chain.
#[test]
fn ledger_appends_at_once_leave_one_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.jsonl");
    let threads: Vec<_> = (0..8).map(|i| {
        let path = path.clone();
        std::thread::spawn(move || {
            for j in 0..10 {
                torq_broker::ledger::Ledger::open(&path).append(torq_broker::ledger::NewEntry {
                    actor: "user".into(), approved_by: None, kind: "refused".into(), tier: "system".into(), tainted: false,
                    reason: format!("{i}-{j}"), data: serde_json::json!({}),
                }).unwrap();
            }
        })
    }).collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(torq_broker::ledger::Ledger::open(&path).verify().unwrap(), 80);
}

/// An entry that lands while a commit confirmed builds (break-glass takes no operation's lock) must
/// not disarm its timer: the rollback is what saves a change that cut off a remote machine.
#[test]
fn break_glass_during_a_build_leaves_the_commit_confirmed_timer_armed() {
    let build = format!("{} --repo DIR/none --state-dir DIR/state break-glass --by alice --reason mid-build > /dev/null; {BUILD}", env!("CARGO_BIN_EXE_torq-broker"));
    let m = Confirmable::new_with(&build, ACTIVATE, SCHEDULE);
    let anchor = m.b.repo.head().unwrap();
    let seq = m.propose_theme();
    let a = m.b.apply_confirmed(seq, "casey", false, 1).unwrap();
    assert_eq!(a.body.seq, seq + 2, "the entry landed mid-build: {}", m.log());
    assert_eq!(m.b.status().unwrap().disagreements, Vec::<String>::new(), "the trailer is one short: the commit is what counts");
    // What the timer was told, as the timer would say it.
    let named: u64 = m.log().lines().find_map(|l| l.strip_prefix("schedule ")?.split(' ').next()?.parse().ok()).unwrap();
    let r = m.b.expire_confirm(Some(named), false).unwrap().expect("rolled back");
    assert_eq!(r.body.kind, "rolled_back");
    m.assert_back(&anchor);
}

/// A proposal that failed may be applied again, while the first attempt's timer still waits: that
/// timer is not this attempt's, and has no say over it.
#[test]
fn an_earlier_attempts_timer_has_no_say_over_a_retry() {
    let activate = format!("test -e DIR/second && {ACTIVATE}");
    let m = Confirmable::new_with(BUILD, &activate, SCHEDULE);
    let seq = m.propose_theme();
    assert!(m.b.apply_confirmed(seq, "casey", false, 1).is_err());
    fs::write(m.dir.path().join("second"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let a = m.b.apply_confirmed(seq, "casey", false, 600).unwrap();
    let named: Vec<u64> = m.log().lines().filter_map(|l| l.strip_prefix("schedule ")?.split(' ').next()?.parse().ok()).collect();
    assert_eq!(named.len(), 2, "{}", m.log());
    assert_ne!(named[0], named[1], "each attempt its own: systemd refuses a unit name twice");
    assert!(m.b.expire_confirm(Some(named[0]), false).unwrap().is_none(), "the first attempt's timer finds nothing");
    assert_eq!(a.body.data["confirm"]["timer"], named[1]);
    assert!(m.b.expire_confirm(Some(named[1]), false).unwrap().is_some(), "its own does");
}

/// Before a machine's ledger begins, an entry would be #1, and setup would take it for set up.
#[test]
fn break_glass_is_refused_before_the_ledger_begins() {
    let (dir, repo) = config_dir();
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    assert!(format!("{:#}", b.break_glass("alice", "why").unwrap_err()).contains("not set up"));
    assert!(b.ledger.read_all().unwrap().is_empty());
    b.init().unwrap();
    assert_eq!(b.ledger.read_all().unwrap()[0].body.kind, "init");
}

/// ADR-038: a snapshot before each system change, named in its entry; one that fails is said and stops
/// nothing. Not for a person's own settings: anyone could then push everyone's restore points out.
#[test]
fn a_system_change_records_its_snapshot_and_a_failed_one_stops_nothing() {
    let m = Confirmable::new();
    let seen = m.dir.path().join("taken");
    let state = m.dir.path().join("state");
    let system = write_diff(m.dir.path(), "sys.diff", "--- /dev/null\n+++ b/nixos/services.json\n@@ -0,0 +1 @@\n+{\"openssh\": {\"enable\": true}}\n");
    let b = Broker::new(m.b.repo.root(), &state, "true").with_snapshots(format!("echo {{label}} >> {}", seen.display()));
    // A person's own: none.
    let own = write_diff(m.dir.path(), "own.diff", "--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"hello\"]\n");
    let p = b.propose(&own, "Hello", Actor::User, false).unwrap().body.seq;
    let a = b.apply(p, "casey", false).unwrap();
    assert_eq!(a.body.tier, "user");
    assert!(a.body.data.get("snapshot").is_none() && !seen.exists(), "{:?}", a.body.data);
    // A system change: taken before the entry was written, and its revert takes one too.
    let p = b.propose(&system, "SSH on", Actor::User, false).unwrap().body.seq;
    let a = b.apply(p, "casey", false).unwrap();
    let label = a.body.data["snapshot"]["label"].as_str().unwrap().to_string();
    assert_eq!(fs::read_to_string(&seen).unwrap().trim(), label);
    let r = b.revert(a.body.seq, "casey").unwrap();
    assert!(r.body.data["snapshot"]["label"].is_string(), "{:?}", r.body.data);
    // None configured: nothing in the entry.
    let p = m.b.propose(&system, "SSH on", Actor::User, false).unwrap().body.seq;
    let a = m.b.apply(p, "casey", false).unwrap();
    assert!(a.body.data.get("snapshot").is_none());
    m.b.revert(a.body.seq, "casey").unwrap();
    // It fails: the change goes through, and the entry says why there is none.
    let b = Broker::new(m.b.repo.root(), &state, "true").with_snapshots("false");
    let p = b.propose(&system, "SSH on", Actor::User, false).unwrap().body.seq;
    let a = b.apply(p, "casey", false).unwrap();
    assert!(a.body.data["snapshot"]["error"].as_str().unwrap().contains("snapshot command failed"), "{:?}", a.body.data);
    b.ledger.verify().unwrap();
}

/// A restore holds the lock as long as its copy takes: not while a rollback may need it.
#[test]
fn no_restore_while_a_commit_confirmed_waits() {
    let m = Confirmable::new();
    let seq = m.propose_theme();
    m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    let e = format!("{:#}", m.b.restore_data(seq, "/home/alice/a", "casey").unwrap_err());
    assert!(e.contains("commit confirmed is waiting"), "{e}");
}

/// N3: a power cut in the middle of an append left a line with no end, and every command after it,
/// recovery too, refused. Read past, said by `status`, and gone at the next entry, its bytes in the
/// entry that says so. A whole line that does not verify is an edit, and nothing touches it.
#[test]
fn a_torn_last_line_is_mended_and_an_edit_is_not() {
    use std::io::Write;
    let torn = |bytes: &[u8]| {
        let (dir, repo) = config_dir();
        let b = Broker::new(&repo, dir.path().join("state"), "true");
        b.init().unwrap();
        let mut f = fs::OpenOptions::new().append(true).open(b.ledger.path()).unwrap();
        f.write_all(bytes).unwrap();
        (dir, b)
    };
    let cut = br#"{"seq":2,"ts":"2026-11-06T00:00:00Z","actor":"us"#;
    // Cut short, cut in the middle of a character, and the zeros a file system can leave.
    for tail in [&cut[..], "{\"reason\":\"é".as_bytes().split_last().unwrap().1, &[0u8; 64][..]] {
        let (_dir, b) = torn(tail);
        // Read past: what looks at the ledger sees what was finished, and is told.
        assert_eq!(b.ledger.verify().unwrap(), 1, "{tail:?}");
        let s = b.status().unwrap();
        assert!(s.problems().iter().any(|p| p.contains("cut short")) && !s.drift().iter().any(|p| p.contains("cut short")), "{:?}", s.problems());
        // Mended at the next entry, whoever writes it: break-glass, the way back, above all.
        let (e, _) = b.break_glass("alice", "after the power cut").unwrap();
        let all = b.ledger.read_all().unwrap();
        assert_eq!((all[1].body.kind.as_str(), all[1].body.actor.as_str(), e.body.seq), ("ledger_mended", "broker", 3));
        assert_eq!(all[1].body.data["bytes"], tail.len());
        assert_eq!(all[1].body.data["tail"].as_str().unwrap(), hex::encode(tail), "what the line said is kept");
        b.ledger.verify().unwrap();
        assert!(!b.status().unwrap().problems().iter().any(|p| p.contains("cut short")));
    }
    // A whole line that is not an entry is an edit: refused, and left exactly as it was.
    let (_dir, b) = torn(b"{\"seq\":2}\n");
    assert!(b.ledger.verify().is_err());
    assert!(b.break_glass("alice", "why").is_err());
    assert!(b.ledger.mend().unwrap().is_none());
    // An edit with a torn line after it: nothing removed, whatever asks. It is evidence.
    let (_dir, b) = torn(b"{\"seq\":2}\n{\"cut");
    let before = fs::read(b.ledger.path()).unwrap();
    assert!(b.ledger.mend().is_err() && b.break_glass("alice", "why").is_err());
    assert_eq!(fs::read(b.ledger.path()).unwrap(), before);
    // A whole entry that lost its newline stands: the hash vouches for it. It is not a power cut's.
    let (_dir, b) = torn(b"");
    let e = b.break_glass("alice", "kept").unwrap().0;
    let whole = fs::read(b.ledger.path()).unwrap();
    fs::write(b.ledger.path(), &whole[..whole.len() - 1]).unwrap();
    assert!(b.ledger.mend().unwrap().is_none());
    assert_eq!(fs::read(b.ledger.path()).unwrap(), whole);
    assert_eq!(b.ledger.read_all().unwrap().last().unwrap().hash, e.hash);
}

/// A machine's first entry cut short: it must still start as a new machine, with `init` as its first
/// entry, which setup and the agent's refusals go by. The torn bytes are kept beside the ledger.
#[test]
fn a_torn_first_line_leaves_a_new_machine() {
    let (dir, repo) = config_dir();
    let state = dir.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let b = Broker::new(&repo, &state, "true");
    fs::write(b.ledger.path(), b"{\"seq\":1,\"ts").unwrap();
    assert!(b.ledger.mend().unwrap().is_none());
    assert_eq!(fs::read(b.ledger.path()).unwrap(), b"");
    let kept: Vec<_> = fs::read_dir(&state).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains("torn-")).collect();
    assert_eq!(fs::read(kept[0].path()).unwrap(), b"{\"seq\":1,\"ts");
    b.init().unwrap();
    assert_eq!(b.ledger.read_all().unwrap()[0].body.kind, "init");
}

/// The password set in the running system and then not kept: the ledger must not say it was not changed.
#[test]
fn a_password_set_but_not_kept_is_said_as_that() {
    let (dir, repo) = config_dir();
    let pw = dir.path().join("pw");
    let people = vec!["casey".to_string()];
    // `cat`, which reads what it is given: `true` exits first, and the write then fails, or not, by luck.
    let b = Broker::new(&repo, dir.path().join("state"), "true").with_passwords(&pw, "cat").with_people(&people);
    b.init().unwrap();
    // A folder at the name: the rename fails, after the running system has it.
    fs::create_dir_all(pw.join("casey/x")).unwrap();
    assert!(b.set_password("casey", "$6$other$hash", "casey").is_err());
    let all = b.ledger.read_all().unwrap();
    let last = all.last().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.data["running"].as_bool()), ("password_failed", Some(true)));
    assert!(last.body.reason.contains("set in the running system"), "{}", last.body.reason);
    // Failing before the running system has it says the other thing.
    fs::remove_dir_all(pw.join("casey")).unwrap();
    let no = Broker::new(&repo, dir.path().join("state"), "true").with_passwords(&pw, "false").with_people(&people);
    assert!(no.set_password("casey", "$6$other$hash", "casey").is_err());
    let all = no.ledger.read_all().unwrap();
    assert_eq!(all.last().unwrap().body.data["running"].as_bool(), Some(false));
    assert!(!pw.join("casey").exists() && !pw.join(".casey.new").exists(), "nothing kept");
}

/// The ledger and the repository each say who approved what: rewriting one alone is seen. By the entry's
/// commit, so an entry landing during an apply, and a rolled-back commit, raise nothing.
#[test]
fn the_ledger_is_checked_against_the_repository() {
    let m = Confirmable::new();
    let p = m.propose_theme();
    let a = m.b.apply(p, "casey", false).unwrap();
    let r = m.b.revert(a.body.seq, "casey").unwrap();
    assert_eq!(m.b.status().unwrap().disagreements, Vec::<String>::new());
    // Rolled back: its commit gone from history is no disagreement.
    let p = m.propose_theme();
    let c = m.b.apply_confirmed(p, "casey", false, 1).unwrap();
    m.b.expire_confirm(Some(c.body.seq), false).unwrap().expect("rolled back");
    assert_eq!(m.b.status().unwrap().disagreements, Vec::<String>::new());
    // The commit rewritten to say someone else approved it.
    let commit = r.body.data["commit"].as_str().unwrap();
    let msg = git(&m.b, &["log", "-1", "--format=%B", commit]).replace("Approved-By: casey", "Approved-By: mallory");
    let tree = git(&m.b, &["rev-parse", &format!("{commit}^{{tree}}")]);
    let parent = git(&m.b, &["rev-parse", &format!("{commit}^")]);
    let forged = git(&m.b, &["-c", "user.name=m", "-c", "user.email=m@m", "commit-tree", tree.trim(), "-p", parent.trim(), "-m", msg.trim()]);
    git(&m.b, &["update-ref", "refs/heads/main", forged.trim()]);
    let d = m.b.status().unwrap().disagreements;
    assert!(d.iter().any(|x| x.contains(&format!("#{}", r.body.seq)) && x.contains("not in the machine")), "{d:?}");
}

/// The ledger rewritten alone, chain and all, as root could: the repository still says otherwise. And
/// a person who has looked says so, on the record, and it is said no more until there is something new.
#[test]
fn a_ledger_rewritten_alone_is_seen_and_a_person_can_say_they_looked() {
    let m = Confirmable::new();
    let p = m.propose_theme();
    let a = m.b.apply(p, "casey", false).unwrap();
    let path = m.b.ledger.path().to_path_buf();
    let rewrite = |approved: &str, proposal: u64| {
        let forged = path.with_extension("forged");
        let _ = fs::remove_file(&forged);
        let l = Ledger::open(&forged);
        for e in m.b.ledger.read_all().unwrap() {
            let mut b = e.body;
            if b.seq == a.body.seq {
                b.approved_by = Some(approved.into());
                b.data["proposal"] = proposal.into();
            }
            l.append(NewEntry { actor: b.actor, approved_by: b.approved_by, kind: b.kind, tier: b.tier, tainted: b.tainted, reason: b.reason, data: b.data }).unwrap();
        }
        fs::read(&forged).unwrap()
    };
    let real = fs::read(&path).unwrap();
    fs::write(&path, rewrite("mallory", p)).unwrap();
    m.b.ledger.verify().unwrap();
    let d = m.b.status().unwrap().disagreements;
    assert!(d.iter().any(|x| x.contains("says mallory approved it")), "{d:?}");
    fs::write(&path, rewrite("casey", p + 40)).unwrap();
    let s = m.b.status().unwrap();
    assert!(s.disagreements.iter().any(|x| x.contains("its commit says")), "{:?}", s.disagreements);
    // No rebuild puts this right: it is not the machine's drift, and repair says there is nothing to do.
    assert!(s.drift().is_empty() && !s.problems().is_empty());
    assert!(format!("{:#}", m.b.repair("casey").unwrap_err()).contains("nothing to repair"));
    // Looked into: said no more, on the record.
    let e = m.b.acknowledge_disagreements("casey").unwrap();
    assert_eq!(e.body.data["entries"], serde_json::json!([a.body.seq]));
    assert_eq!(m.b.status().unwrap().disagreements, Vec::<String>::new());
    assert!(m.b.acknowledge_disagreements("casey").is_err(), "nothing left to acknowledge");
    // Something else wrong with the same entry is new: said, whatever was looked into before.
    let looked = fs::read(&path).unwrap();
    fs::write(&path, rewrite("mallory", p + 40)).unwrap();
    let d = m.b.status().unwrap().disagreements;
    assert!(d.iter().any(|x| x.contains("says mallory approved it")), "{d:?}");
    fs::write(&path, looked).unwrap();
    fs::write(&path, real).unwrap();
}

/// By hand: one taken is listed and named by its entry; one deleted is on the record first, whatever
/// becomes of the deleting. On a machine with none, each says so and writes nothing.
#[test]
fn a_snapshot_by_hand_and_one_deleted() {
    let (dir, repo) = config_dir();
    let (state, snaps) = (dir.path().join("state"), dir.path().join("snaps"));
    fs::create_dir_all(&snaps).unwrap();
    let cmd = format!("case {{action}} in take) mkdir {0}/{{label}} ;; delete) rm -r {0}/{{label}} ;; esac", snaps.display());
    let b = Broker::new(&repo, &state, "true").with_snapshots(cmd).with_snapshot_dir(&snaps);
    b.init().unwrap();
    let e = b.snapshot_now("casey").unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.approved_by.as_deref()), ("snapshot_taken", Some("casey")));
    let list = b.snapshots().unwrap();
    assert_eq!((list.len(), list[0].change), (1, e.body.seq));
    let d = b.delete_snapshot(e.body.seq, "casey").unwrap();
    assert_eq!(d.body.kind, "snapshot_deleted");
    assert!(b.snapshots().unwrap().is_empty());
    assert!(format!("{:#}", b.delete_snapshot(e.body.seq, "casey").unwrap_err()).contains("already gone"));
    assert!(format!("{:#}", b.delete_snapshot(1, "casey").unwrap_err()).contains("has no snapshot"));
    // It will not go: what was said stands, and what happened is said after it.
    let keep = Broker::new(&repo, &state, "true").with_snapshots(format!("case {{action}} in take) mkdir {0}/{{label}} ;; delete) false ;; esac", snaps.display())).with_snapshot_dir(&snaps);
    let e = keep.snapshot_now("casey").unwrap();
    assert!(keep.delete_snapshot(e.body.seq, "casey").is_err());
    let all = keep.ledger.read_all().unwrap();
    let [.., said, failed] = all.as_slice() else { panic!() };
    assert_eq!((said.body.kind.as_str(), failed.body.kind.as_str()), ("snapshot_deleted", "snapshot_delete_failed"));
    assert!(failed.body.reason.contains("still there"), "{}", failed.body.reason);
    // A snapshot that fails is no entry.
    let n = keep.ledger.read_all().unwrap().len();
    let bad = Broker::new(&repo, &state, "true").with_snapshots("false").with_snapshot_dir(&snaps);
    assert!(format!("{:#}", bad.snapshot_now("casey").unwrap_err()).contains("no snapshot was taken"));
    assert_eq!(bad.ledger.read_all().unwrap().len(), n);
    // None here at all.
    let none = Broker::new(&repo, &state, "true");
    assert!(format!("{:#}", none.snapshot_now("casey").unwrap_err()).contains("does not take snapshots"));
    assert!(format!("{:#}", none.delete_snapshot(e.body.seq, "casey").unwrap_err()).contains("does not take snapshots"));
}

/// The audit of M3, H1: a diff that is nothing to another's file at one commit and something at the
/// next. Its own folder at the gate, and written over theirs at apply, with no password. Whatever the
/// socket requires, apply refuses a proposal that no longer does what was judged.
#[test]
fn a_proposal_that_now_does_more_than_was_judged_is_refused() {
    let (dir, repo) = config_dir();
    let list = "[\n  \"hello\",\n  \"jq\",\n  \"tree\"\n]\n";
    for who in ["dev", "sam"] {
        fs::create_dir_all(repo.join("users").join(who)).unwrap();
        fs::write(repo.join("users").join(who).join("packages.json"), list).unwrap();
    }
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    b.init().unwrap();
    let crafted = write_diff(dir.path(), "crafted.diff", "diff --git a/users/dev/packages.json b/users/dev/packages.json\ndeleted file mode 100644\n--- a/users/dev/packages.json\n+++ /dev/null\n@@ -1,5 +0,0 @@\n-[\n-  \"hello\",\n-  \"jq\",\n-  \"tree\"\n-]\ndiff --git a/users/sam/packages.json b/users/dev/packages.json\nsimilarity index 100%\ncopy from users/sam/packages.json\ncopy to users/dev/packages.json\ndiff --git a/users/sam/packages.json b/users/sam/packages.json\n--- a/users/sam/packages.json\n+++ b/users/sam/packages.json\n@@ -4,2 +4,2 @@\n-  \"tree\"\n+  \"tmux\"\n ]\n");
    let p = b.propose(&crafted, "mine", Actor::User, false).unwrap();
    assert_eq!(p.body.tier, "user", "as the gate reads it: {:?}", p.body.data["files"]);
    // The configuration moves on, by a change of the proposer's own.
    let own = write_diff(dir.path(), "own.diff", "--- a/users/sam/packages.json\n+++ b/users/sam/packages.json\n@@ -2,3 +2,3 @@\n   \"hello\",\n-  \"jq\",\n+  \"vim\",\n   \"tree\"\n");
    let o = b.propose(&own, "mine too", Actor::User, false).unwrap().body.seq;
    b.apply(o, "sam", false).unwrap();
    let e = format!("{:#}", b.apply(p.body.seq, "sam", false).unwrap_err());
    assert!(e.contains("no longer does what it was judged on"), "{e}");
    assert_eq!(fs::read_to_string(repo.join("users/dev/packages.json")).unwrap(), list, "the other person's list");
    assert_eq!(kinds(&b).iter().filter(|k| *k == "apply_refused").count(), 1);
    // Again and again: nothing more on the record, and still refused.
    assert!(b.apply(p.body.seq, "sam", false).is_err());
    assert_eq!(kinds(&b).iter().filter(|k| *k == "apply_refused").count(), 1);
    b.ledger.verify().unwrap();
}

/// ADR-053 (b): the same diff, proposed with its base, as the socket requires. On the moved head its
/// effect is not the one recorded (nothing to the other list then, its whole content now): refused by
/// the re-check, before files and tier are even compared.
#[test]
fn the_audits_copy_with_a_base_is_refused_by_the_recheck() {
    let (dir, repo) = config_dir();
    let list = "[\n  \"hello\",\n  \"jq\",\n  \"tree\"\n]\n";
    for who in ["dev", "sam"] {
        fs::create_dir_all(repo.join("users").join(who)).unwrap();
        fs::write(repo.join("users").join(who).join("packages.json"), list).unwrap();
    }
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    b.init().unwrap();
    let crafted = write_diff(dir.path(), "crafted.diff", "diff --git a/users/dev/packages.json b/users/dev/packages.json\ndeleted file mode 100644\n--- a/users/dev/packages.json\n+++ /dev/null\n@@ -1,5 +0,0 @@\n-[\n-  \"hello\",\n-  \"jq\",\n-  \"tree\"\n-]\ndiff --git a/users/sam/packages.json b/users/dev/packages.json\nsimilarity index 100%\ncopy from users/sam/packages.json\ncopy to users/dev/packages.json\ndiff --git a/users/sam/packages.json b/users/sam/packages.json\n--- a/users/sam/packages.json\n+++ b/users/sam/packages.json\n@@ -4,2 +4,2 @@\n-  \"tree\"\n+  \"tmux\"\n ]\n");
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &crafted, "mine", Actor::User, false).unwrap();
    assert_eq!(p.body.tier, "user");
    let own = write_diff(dir.path(), "own.diff", "--- a/users/sam/packages.json\n+++ b/users/sam/packages.json\n@@ -2,3 +2,3 @@\n   \"hello\",\n-  \"jq\",\n+  \"vim\",\n   \"tree\"\n");
    let o = b.propose(&own, "mine too", Actor::User, false).unwrap().body.seq;
    b.apply(o, "sam", false).unwrap();
    let e = format!("{:#}", b.apply(p.body.seq, "sam", false).unwrap_err());
    assert!(e.contains("would now do something different from what was shown"), "{e}");
    assert_eq!(fs::read_to_string(repo.join("users/dev/packages.json")).unwrap(), list, "the other person's list");
    assert_eq!(kinds(&b).iter().filter(|k| *k == "applied").count(), 1, "only sam's own");
}

/// The audit of M3, M4 (and N19): a proposal's diff and effect are kept beside the ledger by their
/// hash, so the ledger stays small; padding before a diff is refused; the agent's are smaller; and a
/// diff changed where it is kept is refused at apply, not applied.
#[test]
fn diffs_beside_the_ledger_and_no_padding() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let big: String = (0..20_000).map(|i| format!("+line {i}\n")).collect();
    let d = write_diff(dir.path(), "big.diff", &format!("--- /dev/null\n+++ b/users/casey/notes.txt\n@@ -0,0 +1,20000 @@\n{big}"));
    let p = b.propose_on(Some(&head), &d, "notes", Actor::User, false).unwrap();
    let line = fs::read_to_string(b.ledger.path()).unwrap().lines().last().unwrap().len();
    assert!(line < 2000, "the entry is {line} bytes: the diff is not in it");
    assert!(b.proposal_text(&p, "diff").unwrap().contains("+line 19999"));
    // Padding: refused, whoever sends it.
    let padded = write_diff(dir.path(), "pad.diff", &format!("{}\n--- /dev/null\n+++ b/users/casey/x.txt\n@@ -0,0 +1 @@\n+x\n", "x".repeat(1000)));
    for actor in [Actor::User, Actor::Agent] {
        let e = format!("{:#}", b.propose_on(Some(&head), &padded, "pad", actor, false).unwrap_err());
        assert!(e.contains("nothing before it"), "{e}");
    }
    // The agent's: at most a megabyte.
    let huge: String = (0..200_000).map(|i| format!("+line {i}\n")).collect();
    let d = write_diff(dir.path(), "huge.diff", &format!("--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1,200000 @@\n{huge}"));
    assert!(format!("{:#}", b.propose_on(Some(&head), &d, "huge", Actor::Agent, false).unwrap_err()).contains("at most"));
    // Changed where it is kept: refused at apply.
    let sha = p.body.data["diff_sha256"].as_str().unwrap();
    let kept = dir.path().join("state/proposals").join(sha);
    fs::write(&kept, fs::read_to_string(&kept).unwrap().replace("+line 5\n", "+line five\n")).unwrap();
    assert!(format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err()).contains("was changed"));
}

/// The numbers the agent is held to, as numbers: changing one is a decision, not a refactor (the
/// audit of M3, M6).
#[test]
fn the_agents_limits_are_these_numbers() {
    use torq_broker::{AGENT_MAX_DIFF, AGENT_REASON_MAX, AGENT_REFUSALS_AN_HOUR, AGENT_WAITING_LOOK, AGENT_WAITING_MAX, MAX_DIFF};
    assert_eq!((AGENT_REASON_MAX, AGENT_WAITING_MAX, AGENT_WAITING_LOOK, AGENT_REFUSALS_AN_HOUR), (200, 20, 100, 30));
    assert_eq!((MAX_DIFF, AGENT_MAX_DIFF), (4 << 20, 1 << 20));
}

/// A commit confirmed is settled only by an entry after it: one before, naming its number in advance,
/// settles nothing (the audit of M3, M6).
#[test]
fn an_entry_before_a_commit_confirmed_does_not_settle_it() {
    let m = Confirmable::new();
    let seq = m.propose_theme();
    let next = m.b.ledger.read_all().unwrap().len() as u64 + 2;
    m.b.ledger.append(torq_broker::ledger::NewEntry { actor: "user".into(), approved_by: None, kind: "confirmed".into(), tier: "system".into(), tainted: false, reason: "early".into(), data: serde_json::json!({ "applied": next }) }).unwrap();
    let a = m.b.apply_confirmed(seq, "casey", false, 10).unwrap();
    assert_eq!(a.body.seq, next);
    assert!(m.b.status().unwrap().pending_confirm.is_some(), "still waiting");
}

/// The review of 0.2.1, M2: a forbidden path is no one's, by sudo too, not the socket's rule alone.
#[test]
fn a_person_may_not_change_forbidden_paths_either() {
    let (dir, b) = setup("true");
    let head = b.repo.head().unwrap();
    let d = write_diff(dir.path(), "attr.diff", "--- /dev/null\n+++ b/.gitattributes\n@@ -0,0 +1 @@\n+* -diff\n");
    let e = format!("{:#}", b.propose_on(Some(&head), &d, "attributes", Actor::User, false).unwrap_err());
    assert!(e.contains("no one may change forbidden paths"), "{e}");
}

/// The review of 0.2.1, H1: a commit named in a ledger is text whoever wrote the ledger chose, and an
/// option to git given as one (`--output=<file>`) wrote that file as root. Refused at git, and at adopt.
#[test]
fn a_commit_named_in_the_ledger_is_never_an_option_to_git() {
    let (dir, b) = setup("true");
    let target = dir.path().join("written");
    let e = b.ledger.append(torq_broker::ledger::NewEntry { actor: "user".into(), approved_by: None, kind: "proposal".into(), tier: "user".into(), tainted: false, reason: "x".into(), data: serde_json::json!({ "commit": format!("--output={}", target.display()) }) }).unwrap();
    let out = run_cli(dir.path(), "true", &["show", &e.body.seq.to_string()]);
    assert!(!target.exists(), "git wrote {}: {out:?}", target.display());
    assert!(b.repo.show(&format!("--output={}", target.display())).is_err());
    for bad in ["--output=/x", "HEAD~1", "main", "-S", "refs/../x", &"a".repeat(39)] {
        assert!(torq_broker::repo::revision(bad).is_err(), "{bad}");
    }
    for good in ["HEAD", "refs/torq/ledger", &"a".repeat(40)] {
        assert!(torq_broker::repo::revision(good).is_ok(), "{good}");
    }
}

// ── commit at ────────────────────────────────────────────────────────────────────────────────────

/// A broker whose commit-at timer is a stub: arming and stopping it are lines in the log.
struct At {
    m: Confirmable,
}

/// The machine's accounts: casey and viper people, viper an administrator, casey too unless `casey_admin` is false.
fn accounts(dir: &Path, casey_admin: bool) {
    fs::write(dir.join("passwd"), "root:x:0:0::/root:/bin/sh\ncasey:x:1000:100::/home/casey:/bin/sh\nviper:x:1001:100::/home/viper:/bin/sh\ntorq-agent:x:990:990::/var/empty:/bin/false\n").unwrap();
    fs::write(dir.join("group"), format!("wheel:x:1:{}viper\nusers:x:100:\n", if casey_admin { "casey," } else { "" })).unwrap();
}

impl At {
    fn new() -> Self {
        let m = Confirmable::new();
        accounts(m.dir.path(), true);
        At { m }
    }
    fn s(&self) -> torq_broker::at::Scheduler<'_> {
        let log = self.m.dir.path().join("log").display().to_string();
        self.with(&self.m.b, &format!("echo at {{id}} {{at}} {{broker}} >> {log}"), &format!("echo cancel {{id}} >> {log}"))
    }
    /// A scheduler for broker `b` with these commands, and this test's accounts.
    fn with<'a>(&self, b: &'a Broker, schedule: &str, cancel: &str) -> torq_broker::at::Scheduler<'a> {
        torq_broker::at::Scheduler::new(b, schedule, cancel).with_accounts(self.m.dir.path().join("passwd"), self.m.dir.path().join("group"))
    }
    /// A schedule written straight into the ledger, as a ledger from elsewhere might hold one.
    fn forged(&self, proposal: u64, by: &str, at: &str, sha: &str) -> (u64, u64) {
        let timer = 777_000 + proposal;
        let e = self.m.b.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "apply_scheduled".into(),
            tier: "system".into(),
            tainted: false,
            reason: "forged".into(),
            data: serde_json::json!({ "proposal": proposal, "at": at, "timer": timer, "base": self.m.b.repo.head().unwrap(), "diff_sha256": sha }),
        }).unwrap();
        (e.body.seq, timer)
    }
    /// A proposal on the configuration as it is (its base: HEAD).
    fn propose(&self, name: &str, diff: &str) -> u64 {
        let d = write_diff(self.m.dir.path(), name, diff);
        self.m.b.propose_on(Some(&self.m.b.repo.head().unwrap()), &d, name, Actor::User, false).unwrap().body.seq
    }
    fn in_seconds(s: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339()
    }
    fn theme(&self) -> String {
        fs::read_to_string(self.m.b.repo.root().join("users/casey.nix")).unwrap()
    }
}

#[test]
fn a_commit_at_lands_at_its_time_as_the_one_who_approved_it() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.approved_by.as_deref(), e.body.data["proposal"].as_u64()), ("apply_scheduled", Some("casey"), Some(p)));
    assert_eq!(e.body.data["base"], t.m.b.repo.head().unwrap().as_str());
    assert_eq!(e.body.data["diff_sha256"].as_str().map(str::len), Some(64), "the diff approved, by its hash");
    let timer = e.body.data["timer"].as_u64().unwrap();
    // Its own timer, running this broker by its own path, at a wall-clock time.
    let log = t.m.log();
    assert!(log.contains(&format!("at {timer} ")) && log.contains(" UTC /nix/store/old-broker/bin/torq-broker"), "{log}");
    assert!(t.theme().contains("dark"), "nothing applied yet");
    let listed = t.s().scheduled().unwrap();
    assert_eq!((listed.len(), listed[0].seq, listed[0].proposal), (1, e.body.seq, p));

    let a = t.s().run(timer).unwrap().expect("applied");
    assert_eq!((a.body.kind.as_str(), a.body.approved_by.as_deref()), ("applied", Some("casey")));
    assert_eq!((a.body.data["proposal"].as_u64(), a.body.data["scheduled"].as_u64()), (Some(p), Some(e.body.seq)));
    assert!(t.theme().contains("light"));
    assert!(t.s().scheduled().unwrap().is_empty(), "settled");
    // The timer's retry, or a second run, finds nothing to do.
    assert!(t.s().run(timer).unwrap().is_none());
    // An id no schedule names does nothing.
    assert!(t.s().run(12345).unwrap().is_none());
}

#[test]
fn a_commit_at_is_not_run_before_its_time() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(3600)).unwrap();
    let err = format!("{:#}", t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap_err());
    assert!(err.contains("not yet"), "{err}");
    assert_eq!(kinds(&t.m.b).last().map(String::as_str), Some("apply_scheduled"), "nothing ledgered");
    assert!(t.theme().contains("dark"));
}

#[test]
fn a_cancelled_commit_at_never_lands() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    let timer = e.body.data["timer"].as_u64().unwrap();
    let c = t.s().cancel(e.body.seq, "viper").unwrap();
    assert_eq!((c.body.kind.as_str(), c.body.approved_by.as_deref(), c.body.data["scheduled"].as_u64()), ("apply_cancelled", Some("viper"), Some(e.body.seq)));
    assert!(t.m.log().contains(&format!("cancel {timer}")), "its timer stopped: {}", t.m.log());
    assert!(t.s().scheduled().unwrap().is_empty());
    // A timer that could not be stopped still applies nothing: the ledger says it is cancelled.
    assert!(t.s().run(timer).unwrap().is_none());
    assert!(t.theme().contains("dark"));
    assert!(format!("{:#}", t.s().cancel(e.body.seq, "viper").unwrap_err()).contains("not a commit scheduled"));
    // A timer that cannot be stopped does not undo the cancel.
    let p2 = t.propose("theme2", THEME);
    let e2 = t.s().schedule(p2, "casey", &At::in_seconds(30)).unwrap();
    let stuck = t.with(&t.m.b, "", "false");
    stuck.cancel(e2.body.seq, "viper").unwrap();
    assert!(t.s().run(e2.body.data["timer"].as_u64().unwrap()).unwrap().is_none());
    assert!(t.theme().contains("dark"));
}

/// ADR-053 (f): a commit at takes apply's re-check. The configuration moved on in another file: it lands,
/// re-checked; in the same file, so that it would do something else: refused on the record.
#[test]
fn a_commit_at_on_a_moved_configuration_is_rechecked() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    // Another file: the change still does exactly what was shown.
    let other = t.propose("night", NIGHT_LIGHT);
    t.m.b.apply(other, "viper", false).unwrap();
    let moved = t.m.b.repo.head().unwrap();
    let a = t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().expect("applied");
    assert_eq!((a.body.kind.as_str(), a.body.data["scheduled"].as_u64()), ("applied", Some(e.body.seq)));
    assert_eq!(a.body.data["rechecked_on"], serde_json::json!(moved));
    assert!(t.theme().contains("light"));

    // The same file, changed below the line it touches: it would now do something else.
    let q = t.propose("night off", "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ nightLight = true; }\n+{ nightLight = false; }\n");
    let e = t.s().schedule(q, "casey", &At::in_seconds(30)).unwrap();
    let note = t.propose("note", "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1,2 @@\n { nightLight = true; }\n+# a note\n");
    t.m.b.apply(note, "viper", false).unwrap();
    let r = t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().expect("refused");
    assert_eq!((r.body.kind.as_str(), r.body.data["scheduled"].as_u64()), ("apply_refused", Some(e.body.seq)));
    assert!(r.body.reason.contains("would now do something different"), "{}", r.body.reason);
    assert!(fs::read_to_string(t.m.b.repo.root().join("desktop.nix")).unwrap().contains("nightLight = true"), "never applied");
    assert!(t.s().scheduled().unwrap().is_empty() && t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().is_none());
}

/// Scheduling on a moved configuration takes the same re-check, at once: one that still does what was
/// shown is scheduled, and its entry says it was re-checked.
#[test]
fn a_commit_at_scheduled_on_a_moved_configuration_is_rechecked() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let other = t.propose("night", NIGHT_LIGHT);
    t.m.b.apply(other, "viper", false).unwrap();
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    assert_eq!(e.body.data["rechecked_on"], serde_json::json!(t.m.b.repo.head().unwrap()));
}

#[test]
fn a_commit_confirmed_waiting_holds_a_commit_at_back() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    let other = t.m.b.propose(&write_diff(t.m.dir.path(), "night", NIGHT_LIGHT), "night", Actor::User, false).unwrap().body.seq;
    t.m.b.apply_confirmed(other, "viper", false, 10).unwrap();
    let r = t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().expect("refused");
    assert_eq!(r.body.kind, "apply_refused");
    assert!(r.body.reason.contains("commit confirmed"), "{}", r.body.reason);
    assert!(t.theme().contains("dark"));
}

#[test]
fn a_commit_at_applies_only_the_diff_approved() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    // The diff kept beside the ledger, changed after the approval.
    let pe = t.m.b.ledger.get(p).unwrap();
    let kept = t.m.dir.path().join("state/proposals").join(pe.body.data["diff_sha256"].as_str().unwrap());
    fs::write(&kept, THEME.replace("light", "evil")).unwrap();
    let r = t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().expect("refused");
    assert_eq!(r.body.kind, "apply_refused");
    assert!(r.body.reason.contains("changed"), "{}", r.body.reason);
    assert!(t.theme().contains("dark"));
}

#[test]
fn one_commit_at_at_a_time_and_only_within_the_bounds() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    for (at, why) in [
        (At::in_seconds(-60), "not in the future"),
        (At::in_seconds(31 * 86_400), "more than 30 days"),
        ("tomorrow".to_string(), "not an RFC 3339 time"),
    ] {
        let err = format!("{:#}", t.s().schedule(p, "casey", &at).unwrap_err());
        assert!(err.contains(why), "{at}: {err}");
    }
    let e = t.s().schedule(p, "casey", &At::in_seconds(30 * 86_400 - 60)).unwrap();
    let q = t.propose("night", NIGHT_LIGHT);
    let err = format!("{:#}", t.s().schedule(q, "casey", &At::in_seconds(60)).unwrap_err());
    assert!(err.contains("already scheduled") && err.contains("one at a time"), "{err}");
    t.s().cancel(e.body.seq, "casey").unwrap();
    t.s().schedule(q, "casey", &At::in_seconds(60)).unwrap();
    assert_eq!(kinds(&t.m.b).iter().filter(|k| *k == "apply_scheduled").count(), 2, "refusals at schedule time are not ledgered");
}

#[test]
fn a_commit_at_is_a_fresh_untainted_proposal_with_a_base() {
    let t = At::new();
    let d = write_diff(t.m.dir.path(), "theme", THEME);
    let no_base = t.m.b.propose(&d, "theme", Actor::User, false).unwrap().body.seq;
    assert!(format!("{:#}", t.s().schedule(no_base, "casey", &At::in_seconds(60)).unwrap_err()).contains("its base"));
    let tainted = t.m.b.propose_on(Some(&t.m.b.repo.head().unwrap()), &d, "theme", Actor::User, true).unwrap().body.seq;
    assert!(format!("{:#}", t.s().schedule(tainted, "casey", &At::in_seconds(60)).unwrap_err()).contains("untrusted"));
    let p = t.propose("theme2", THEME);
    let applied = t.m.b.apply(p, "casey", false).unwrap().body.seq;
    assert!(format!("{:#}", t.s().schedule(p, "casey", &At::in_seconds(60)).unwrap_err()).contains("already applied"));
    // Made on the theme as applied, which the revert takes back: it would now do something else (ADR-053).
    let stale = t.propose("blue", "--- a/users/casey.nix\n+++ b/users/casey.nix\n@@ -1 +1 @@\n-{ theme = \"light\"; }\n+{ theme = \"blue\"; }\n");
    t.m.b.revert(applied, "casey").unwrap();
    assert!(format!("{:#}", t.s().schedule(stale, "casey", &At::in_seconds(60)).unwrap_err()).contains("would now do something different"));
    assert!(format!("{:#}", t.s().schedule(stale, "not a name!", &At::in_seconds(60)).unwrap_err()).contains("not a person's name"));
    let none = t.with(&t.m.b, "", "");
    let fresh = t.propose("night2", NIGHT_LIGHT);
    assert!(format!("{:#}", none.schedule(fresh, "casey", &At::in_seconds(60)).unwrap_err()).contains("schedules no commits"));
    // A timer that cannot be armed: nothing recorded, nothing scheduled.
    let broken = t.with(&t.m.b, "false", "");
    assert!(broken.schedule(fresh, "casey", &At::in_seconds(60)).is_err());
    assert!(t.s().scheduled().unwrap().is_empty());
}

#[test]
fn a_failed_commit_at_is_ledgered_once_as_scheduled() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    let failing = Broker::new(t.m.b.repo.root(), t.m.dir.path().join("state"), "false");
    let s = t.with(&failing, "", "");
    let timer = e.body.data["timer"].as_u64().unwrap();
    assert!(s.run(timer).is_err());
    let f = failing.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((f.body.kind.as_str(), f.body.data["scheduled"].as_u64(), f.body.approved_by.as_deref()), ("apply_failed", Some(e.body.seq), Some("casey")));
    assert!(s.run(timer).unwrap().is_none(), "settled: the timer's retry does nothing");
    assert!(t.theme().contains("dark"));
}

#[test]
fn after_a_restart_a_commit_at_is_armed_again_or_refused_if_its_time_passed() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(2)).unwrap();
    let timer = e.body.data["timer"].as_u64().unwrap();
    // The same boot: its timer is there, nothing to do.
    assert!(t.s().resume().unwrap().is_empty());
    assert_eq!(t.m.log().matches(&format!("at {timer} ")).count(), 1);
    // A restart while it was still ahead: armed again, under the same id.
    fs::write(t.m.dir.path().join("boot_id"), "boot-2\n").unwrap();
    assert!(t.s().resume().unwrap().is_empty());
    assert_eq!(t.m.log().matches(&format!("at {timer} ")).count(), 2, "{}", t.m.log());
    // A restart after its time: refused, never applied late.
    std::thread::sleep(std::time::Duration::from_millis(2500));
    fs::write(t.m.dir.path().join("boot_id"), "boot-3\n").unwrap();
    let r = t.s().resume().unwrap();
    assert_eq!((r.len(), r[0].body.kind.as_str(), r[0].body.data["scheduled"].as_u64()), (1, "apply_refused", Some(e.body.seq)));
    assert!(r[0].body.reason.contains("not running at its time"), "{}", r[0].body.reason);
    assert!(t.s().run(timer).unwrap().is_none() && t.theme().contains("dark"));
}

#[test]
fn the_scheduled_commits_are_listed_by_the_broker_binary() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(600)).unwrap();
    let out = t.m.cli(BUILD, ACTIVATE, &["scheduled"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!((v["scheduled"][0]["seq"].as_u64(), v["scheduled"][0]["proposal"].as_u64(), v["scheduled"][0]["approved_by"].as_str()), (Some(e.body.seq), Some(p), Some("casey")));
    assert_eq!(v["scheduled"][0]["at"], e.body.data["at"]);
}

/// The review of commit at: what was approved for one time is not approved for another.
#[test]
fn a_commit_at_long_past_its_time_is_refused_and_blocks_no_one() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let sha = t.m.b.ledger.get(p).unwrap().body.data["diff_sha256"].as_str().unwrap().to_string();
    let late = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
    let (seq, timer) = t.forged(p, "casey", &late, &sha);
    let r = t.s().run(timer).unwrap().expect("refused");
    assert_eq!((r.body.kind.as_str(), r.body.data["scheduled"].as_u64()), ("apply_refused", Some(seq)));
    assert!(r.body.reason.contains("minutes past its time"), "{}", r.body.reason);
    assert!(t.theme().contains("dark"));
    // Within the grace, it runs.
    let (_, timer) = t.forged(p, "casey", &(chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339(), &sha);
    assert_eq!(t.s().run(timer).unwrap().unwrap().body.kind, "applied");
    // One long gone that never ran is settled by the next schedule, on the record, not in its way.
    let q = t.propose("night", NIGHT_LIGHT);
    let (stuck, _) = t.forged(q, "casey", &late, &"0".repeat(64));
    t.s().schedule(q, "casey", &At::in_seconds(60)).unwrap();
    let r = t.m.b.ledger.read_all().unwrap().into_iter().find(|e| e.body.kind == "apply_refused" && e.body.data["scheduled"] == stuck).unwrap();
    assert!(r.body.reason.contains("its time passed"), "{}", r.body.reason);
}

/// The approver must still be someone who may: an account, a person's, an administrator unless the
/// change is wholly their own folder.
#[test]
fn a_commit_at_whose_approver_may_no_longer_is_refused() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let e = t.s().schedule(p, "casey", &At::in_seconds(30)).unwrap();
    accounts(t.m.dir.path(), false);
    let r = t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().expect("refused");
    assert_eq!(r.body.kind, "apply_refused");
    assert!(r.body.reason.contains("no longer an administrator"), "{}", r.body.reason);
    assert!(t.theme().contains("dark"));
    // Their own folder needs no administrator.
    let own = t.propose("own", &new_user_file("users/casey/packages.json", "[\"hello\"]"));
    let e = t.s().schedule(own, "casey", &At::in_seconds(30)).unwrap();
    assert_eq!(t.s().run(e.body.data["timer"].as_u64().unwrap()).unwrap().unwrap().body.kind, "applied");
    // No account by that name any more; the agent's; no name at all.
    for (by, why) in [("gone", "not who they were"), ("torq-agent", "not a person's account"), ("not a name!", "names no person")] {
        let q = t.propose(&format!("night-{}", by.len()), NIGHT_LIGHT);
        let sha = t.m.b.ledger.get(q).unwrap().body.data["diff_sha256"].as_str().unwrap().to_string();
        let (seq, timer) = t.forged(q, by, &At::in_seconds(30), &sha);
        let r = t.s().run(timer).unwrap().expect("refused");
        assert_eq!((r.body.kind.as_str(), r.body.data["scheduled"].as_u64()), ("apply_refused", Some(seq)), "{by}");
        assert!(r.body.reason.contains(why), "{by}: {}", r.body.reason);
    }
    assert!(fs::read_to_string(t.m.b.repo.root().join("desktop.nix")).unwrap().contains("false"));
}

/// The hash the approval recorded is compared at the run: a schedule naming another diff than its
/// proposal's (here written straight into the ledger) applies nothing, though the proposal's own
/// text is intact.
#[test]
fn a_commit_at_naming_another_diff_than_its_proposals_is_refused() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let (seq, timer) = t.forged(p, "casey", &At::in_seconds(30), &"a".repeat(64));
    let r = t.s().run(timer).unwrap().expect("refused");
    assert_eq!((r.body.kind.as_str(), r.body.data["scheduled"].as_u64()), ("apply_refused", Some(seq)));
    assert!(r.body.reason.contains("not the one approved"), "{}", r.body.reason);
    assert!(t.theme().contains("dark"));
}

/// At boot, one schedule that cannot be read or armed again is refused on the record; the others go on.
#[test]
fn after_a_restart_one_bad_schedule_does_not_stop_the_others() {
    let t = At::new();
    let p = t.propose("theme", THEME);
    let sha = t.m.b.ledger.get(p).unwrap().body.data["diff_sha256"].as_str().unwrap().to_string();
    let (bad, _) = t.forged(p, "casey", "not a time", &sha);
    let (good, timer) = t.forged(p, "casey", &At::in_seconds(600), &sha);
    let r = t.s().resume().unwrap();
    assert_eq!((r.len(), r[0].body.data["scheduled"].as_u64()), (1, Some(bad)), "{r:?}");
    assert!(t.m.log().contains(&format!("at {timer} ")), "the good one armed: {}", t.m.log());
    // An arming that fails: refused, on the record, not left waiting for ever.
    let r = t.with(&t.m.b, "false", "").resume().unwrap();
    assert_eq!((r.len(), r[0].body.data["scheduled"].as_u64()), (1, Some(good)));
    assert!(r[0].body.reason.contains("could not be armed again"), "{}", r[0].body.reason);
    assert!(t.s().scheduled().unwrap().is_empty());
}

/// apply's own refusal of a proposal that no longer does what was judged (the audit of M3, H1) is
/// ledgered once — and a schedule's refusal of the same proposal is not that once.
#[test]
fn a_schedules_refusal_does_not_hide_applys_own() {
    let t = At::new();
    // A proposal that says it changes one file while its diff changes another (a ledger from elsewhere).
    let p = t.m.b.ledger.append(NewEntry {
        actor: "user".into(),
        approved_by: None,
        kind: "proposal".into(),
        tier: "system".into(),
        tainted: false,
        reason: "says one thing".into(),
        data: serde_json::json!({ "diff": NIGHT_LIGHT, "files": ["users/casey.nix"], "base": t.m.b.repo.head().unwrap() }),
    }).unwrap().body.seq;
    t.m.b.ledger.append(NewEntry {
        actor: "broker".into(),
        approved_by: Some("casey".into()),
        kind: "apply_refused".into(),
        tier: "system".into(),
        tainted: false,
        reason: "Scheduled change not applied: …".into(),
        data: serde_json::json!({ "proposal": p, "scheduled": 99 }),
    }).unwrap();
    let err = format!("{:#}", t.m.b.apply(p, "casey", false).unwrap_err());
    assert!(err.contains("no longer does what it was judged on"), "{err}");
    let own: Vec<_> = t.m.b.ledger.read_all().unwrap().into_iter().filter(|e| e.body.kind == "apply_refused" && e.body.data["proposal"] == p && e.body.data.get("scheduled").is_none()).collect();
    assert_eq!(own.len(), 1, "apply's own refusal is on the record");
}

/// A person's desktop, saved every few minutes, does not build and switch the machine: only the first save
/// (which may make their folder, checked by the build) and anything else does.
/// A person's saved desktop is COSMIC's own settings only: another program's (a weather applet's location)
/// is refused, by whoever proposes it, so it never reaches a repository every account can read.
#[test]
fn a_saved_desktop_with_another_programs_settings_is_refused() {
    let (cfg, b) = setup("true");
    let before = b.ledger.read_all().unwrap().len();
    let d = write_diff(
        cfg.path(),
        "tempest.diff",
        "--- /dev/null\n+++ b/users/casey/desktop.json\n@@ -0,0 +1 @@\n+{\"cosmic\": {\"com.system76.CosmicPanel.Panel/v1/size\": \"M\", \"com.vintagetechie.CosmicExtAppletTempest/v1/latitude\": \"45.1\"}}\n",
    );
    let e = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Desktop: panel", Actor::User, false).unwrap_err();
    assert!(format!("{e:#}").contains("COSMIC's own settings"), "{e:#}");
    assert_eq!(b.ledger.read_all().unwrap().len(), before, "nothing recorded");
    assert!(!b.repo.root().join("users/casey/desktop.json").exists());
}

/// A desktop saved by an older TorqOS, with another program's settings in it, cleaned by the next save: an undo
/// of that save would put them back, so it is refused (an undo of a person's own change needs no approval, so it
/// would otherwise be the way around the rule), and nothing is recorded.
#[test]
fn an_undo_cannot_put_another_programs_settings_back() {
    let (dir, repo) = config_dir();
    let old = "{\"cosmic\": {\"com.system76.CosmicPanel.Panel/v1/size\": \"M\", \"com.vintagetechie.CosmicExtAppletTempest/v1/latitude\": \"45.1\"}}\n";
    fs::create_dir_all(repo.join("users/casey")).unwrap();
    fs::write(repo.join("users/casey/desktop.json"), old).unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    b.init().unwrap();
    let d = write_diff(
        dir.path(),
        "clean.diff",
        &format!("--- a/users/casey/desktop.json\n+++ b/users/casey/desktop.json\n@@ -1 +1 @@\n-{}+{{\"cosmic\": {{\"com.system76.CosmicPanel.Panel/v1/size\": \"M\"}}}}\n", old),
    );
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Desktop: other programs' settings taken out", Actor::User, false).unwrap();
    let a = b.apply(p.body.seq, "casey", false).unwrap();
    let (head, before) = (b.repo.head().unwrap(), kinds(&b));
    let e = format!("{:#}", b.revert(a.body.seq, "casey").unwrap_err());
    assert!(e.contains("this undo would put back what TorqOS no longer takes") && e.contains("COSMIC's own settings"), "{e}");
    assert_eq!(b.repo.head().unwrap(), head, "the undo's commit is reset");
    assert_eq!(kinds(&b), before, "nothing recorded");
    assert!(!dir.path().join("state/pending.json").exists(), "nothing left in flight");
    assert!(!git(&b, &["for-each-ref", "refs/torq/discarded/"]).is_empty(), "the undo's commit is kept for the audit");
    assert!(!fs::read_to_string(repo.join("users/casey/desktop.json")).unwrap().contains("Tempest"));
    // The broker is left as it was: another change goes through.
    let d = write_diff(dir.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
}

#[test]
fn a_saved_desktop_builds_nothing_after_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("builds");
    let script = dir.path().join("rebuild");
    fs::write(&script, format!("#!/bin/sh\necho built >> {}\n", count.display())).unwrap();
    Command::new("chmod").arg("+x").arg(&script).status().unwrap();
    let (cfg, b) = setup(script.to_str().unwrap());
    let builds = || fs::read_to_string(&count).map(|t| t.lines().count()).unwrap_or(0);
    let commit = |name: &str, body: &str| {
        let d = write_diff(cfg.path(), name, body);
        let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Desktop: panel", Actor::User, false).unwrap();
        assert_eq!(p.body.tier, "user");
        b.apply(p.body.seq, "casey", false).unwrap()
    };
    commit("1.diff", "--- /dev/null\n+++ b/users/casey/desktop.json\n@@ -0,0 +1 @@\n+{\"cosmic\": {\"com.system76.CosmicPanel.Panel/v1/size\": \"M\"}}\n");
    let first = builds();
    assert!(first >= 1, "the first save is built");
    let a = commit(
        "2.diff",
        "--- a/users/casey/desktop.json\n+++ b/users/casey/desktop.json\n@@ -1 +1 @@\n-{\"cosmic\": {\"com.system76.CosmicPanel.Panel/v1/size\": \"M\"}}\n+{\"cosmic\": {\"com.system76.CosmicPanel.Panel/v1/size\": \"L\"}}\n",
    );
    assert_eq!(builds(), first, "a later save builds nothing");
    b.revert(a.body.seq, "casey").unwrap();
    assert_eq!(builds(), first, "nor does its undo");
    assert!(fs::read_to_string(b.repo.root().join("users/casey/desktop.json")).unwrap().contains("\"M\""));
    let d = write_diff(cfg.path(), "nl.diff", NIGHT_LIGHT);
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &d, "Night light", Actor::User, false).unwrap();
    b.apply(p.body.seq, "casey", false).unwrap();
    assert!(builds() > first, "anything else is built");
}

/// ADR-052: a person's secrets' rules are changed by them alone, at the broker itself (sudo, as well as the socket):
/// an administrator's approval applies, schedules and undoes no change to them. Each change builds nothing.
#[test]
fn a_persons_secrets_rules_are_theirs_alone_at_the_broker() {
    let dir = tempfile::tempdir().unwrap();
    let count = dir.path().join("builds");
    let script = dir.path().join("rebuild");
    fs::write(&script, format!("#!/bin/sh\necho built >> {}\n", count.display())).unwrap();
    Command::new("chmod").arg("+x").arg(&script).status().unwrap();
    let (cfg, b) = setup(script.to_str().unwrap());
    let builds = || fs::read_to_string(&count).map(|t| t.lines().count()).unwrap_or(0);
    let first = "{\"gh\": {\"kind\": \"token\", \"https\": {\"hosts\": [\"api.github.com\"]}}}";
    let wider = "{\"gh\": {\"kind\": \"token\", \"https\": {\"hosts\": [\"api.github.com\"]}, \"actors\": [\"torq-agent\"]}}";
    let propose = |name: &str, body: String| {
        let d = write_diff(cfg.path(), name, &body);
        b.propose_on(Some(&b.repo.head().unwrap()), &d, "Secret gh", Actor::User, false).unwrap()
    };
    let p = propose("1.diff", format!("--- /dev/null\n+++ b/users/alice/secrets.json\n@@ -0,0 +1 @@\n+{first}\n"));
    let e = format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(e.contains("theirs alone"), "{e}");
    let applied = b.apply(p.body.seq, "alice", false).unwrap();
    let built = builds();
    let p = propose("2.diff", format!("--- a/users/alice/secrets.json\n+++ b/users/alice/secrets.json\n@@ -1 +1 @@\n-{first}\n+{wider}\n"));
    assert!(b.apply(p.body.seq, "casey", false).is_err());
    let second = b.apply(p.body.seq, "alice", false).unwrap();
    assert_eq!(builds(), built, "a change of the rules builds nothing");
    assert!(format!("{:#}", b.revert(second.body.seq, "casey").unwrap_err()).contains("theirs alone"));
    assert!(format!("{:#}", b.revert(applied.body.seq, "casey").unwrap_err()).contains("theirs alone"));
    b.revert(second.body.seq, "alice").unwrap();
    assert_eq!(builds(), built, "nor does its undo");
    // A name twice is no rules file: refused before it is proposed.
    let d = write_diff(cfg.path(), "3.diff", &format!("--- a/users/alice/secrets.json\n+++ b/users/alice/secrets.json\n@@ -1 +1 @@\n-{first}\n+{{\"t\": {{\"kind\": \"token\"}}, \"t\": {{\"kind\": \"ssh-key\"}}}}\n"));
    assert!(b.propose_on(Some(&b.repo.head().unwrap()), &d, "twice", Actor::User, false).is_err());
}
