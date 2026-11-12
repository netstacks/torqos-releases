//! The values of the secrets (ADR-052): kept encrypted outside the repository, root's alone, the system's and a
//! person's apart, and on the record without the value. systemd-creds is stood in for by a script that takes its
//! arguments and binds each value to its name as the real one does; tests/secrets.nix runs the real one.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use torq_broker::secrets::{Kept, Owner};
use torq_broker::Broker;

const VALUE: &str = "ghp_correct-horse-battery-staple";

/// systemd-creds' arguments, and a value bound to its `--name`: one moved to another name does not decrypt.
const CREDS: &str = r#"#!/bin/sh
cmd=$1; shift
name=""; files=""
for a; do case $a in --name=*) name=${a#--name=};; --*) ;; *) files="$files $a";; esac; done
set -- $files
case $cmd in
  encrypt) [ -n "$FAIL" ] && { echo "no TPM" >&2; exit 1; }; rm -f "$2"; umask 022; { printf 'FAKE %s\n' "$name"; base64; } > "$2" ;;
  decrypt) [ "$(head -n 1 "$1")" = "FAKE $name" ] || { echo "name mismatch" >&2; exit 1; }; tail -n +2 "$1" | base64 -d ;;
  *) exit 2 ;;
esac
"#;

struct Machine {
    dir: tempfile::TempDir,
    b: Broker,
}

impl Machine {
    fn new() -> Self {
        Self::with_creds(CREDS)
    }
    fn with_creds(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
        let creds = dir.path().join("creds");
        fs::write(&creds, script).unwrap();
        fs::set_permissions(&creds, fs::Permissions::from_mode(0o755)).unwrap();
        let b = Broker::new(&repo, dir.path().join("state"), "true").with_secrets(dir.path().join("store"), &creds.to_string_lossy());
        b.init().unwrap();
        Machine { dir, b }
    }
    fn store(&self) -> PathBuf {
        self.dir.path().join("store")
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.b.ledger.path()).unwrap()
    }
}

fn alice() -> Owner {
    Owner::Person("alice".into())
}

#[test]
fn a_value_is_kept_encrypted_private_and_off_the_record() {
    let m = Machine::new();
    let e = m.b.set_secret(&Owner::System, "github-ci", VALUE, "casey").unwrap();
    assert_eq!(e.body.kind, "secret_set");
    assert_eq!((e.body.tier.as_str(), e.body.approved_by.as_deref()), ("system", Some("casey")));
    let file = m.store().join("system/github-ci");
    let kept = fs::read_to_string(&file).unwrap();
    assert!(!kept.contains(VALUE), "kept as systemd-creds made it, never as it was given");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(m.store().join("system")).unwrap().permissions().mode() & 0o777, 0o700);
    let (shown, value) = m.b.show_secret(&Owner::System, "github-ci", "casey").unwrap();
    assert_eq!((shown.body.kind.as_str(), value.as_str()), ("secret_shown", VALUE));
    m.b.delete_secret(&Owner::System, "github-ci", "casey").unwrap();
    assert!(!file.exists());
    let ledger = m.ledger();
    assert!(!ledger.contains(VALUE), "no value on the record");
    assert!(ledger.contains("secret_set") && ledger.contains("secret_shown") && ledger.contains("secret_deleted"));
}

#[test]
fn a_persons_secret_is_kept_apart_and_bound_to_its_name() {
    let m = Machine::new();
    m.b.set_secret(&alice(), "github", "alice's", "alice").unwrap();
    m.b.set_secret(&Owner::System, "github", "the system's", "casey").unwrap();
    let e = m.b.set_secret(&alice(), "deploy", "key", "alice").unwrap();
    assert_eq!((e.body.tier.as_str(), e.body.data["user"].as_str()), ("user", Some("alice")));
    assert_eq!(m.b.show_secret(&alice(), "github", "alice").unwrap().1, "alice's");
    assert_eq!(m.b.show_secret(&Owner::System, "github", "casey").unwrap().1, "the system's");
    assert_eq!(
        m.b.kept_secrets(),
        vec![
            Kept { owner: Owner::System, name: "github".into() },
            Kept { owner: alice(), name: "deploy".into() },
            Kept { owner: alice(), name: "github".into() },
        ]
    );
    // A value put where another's would be is not that one's: it does not decrypt under another name.
    fs::copy(m.store().join("users/alice/github"), m.store().join("system/github")).unwrap();
    let err = format!("{:#}", m.b.show_secret(&Owner::System, "github", "casey").unwrap_err());
    assert!(err.contains("could not decrypt"), "{err}");
}

#[test]
fn a_new_value_takes_the_old_ones_place() {
    let m = Machine::new();
    m.b.set_secret(&alice(), "t", "one", "alice").unwrap();
    m.b.set_secret(&alice(), "t", "two", "alice").unwrap();
    assert_eq!(m.b.show_secret(&alice(), "t", "alice").unwrap().1, "two");
    let left: Vec<_> = fs::read_dir(m.store().join("users/alice")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("t")], "no part-written file left beside it");
}

#[test]
fn what_is_refused_is_not_recorded() {
    let m = Machine::new();
    let before = m.ledger();
    for bad in ["", "../x", "a/b", "-x", ".x"] {
        assert!(m.b.set_secret(&Owner::System, bad, VALUE, "casey").is_err(), "{bad:?}");
    }
    assert!(m.b.set_secret(&Owner::System, "t", "", "casey").is_err(), "no value");
    assert!(m.b.set_secret(&Owner::System, "t", &"x".repeat(16 * 1024 + 1), "casey").is_err(), "too large");
    assert!(m.b.set_secret(&Owner::System, "t", "a\0b", "casey").is_err(), "a NUL");
    assert!(m.b.delete_secret(&Owner::System, "none", "casey").is_err(), "nothing to delete");
    assert!(m.b.show_secret(&alice(), "none", "alice").is_err(), "nothing to show");
    assert!(Owner::of(Some("../root")).is_err() && Owner::of(Some("Alice")).is_err());
    assert_eq!(m.ledger(), before);
    assert!(m.b.kept_secrets().is_empty());
}

#[test]
fn a_value_not_kept_says_so_and_leaves_nothing() {
    let m = Machine::with_creds(&CREDS.replace("[ -n \"$FAIL\" ]", "true"));
    let err = format!("{:#}", m.b.set_secret(&Owner::System, "t", VALUE, "casey").unwrap_err());
    assert!(err.contains("not kept") && err.contains("no TPM"), "{err}");
    let ledger = m.ledger();
    assert!(ledger.contains("secret_set_failed") && !ledger.contains(VALUE), "{ledger}");
    assert_eq!(fs::read_dir(m.store().join("system")).unwrap().count(), 0, "no part-written file");
    assert!(m.b.kept_secrets().is_empty());
}

/// However the broker is reached (sudo, as well as the socket): a person's value is set and read by them alone.
#[test]
fn a_persons_value_is_theirs_alone_whoever_runs_the_broker() {
    let m = Machine::new();
    m.b.set_secret(&alice(), "gh", VALUE, "alice").unwrap();
    for e in [m.b.set_secret(&alice(), "gh", "another", "casey").map(drop), m.b.show_secret(&alice(), "gh", "casey").map(drop)] {
        let e = format!("{:#}", e.unwrap_err());
        assert!(e.contains("theirs alone"), "{e}");
    }
    assert_eq!(m.b.show_secret(&alice(), "gh", "alice").unwrap().1, VALUE, "unchanged");
    // Deleting it is an administrator's too: the gate (socket.rs) says who may.
    m.b.delete_secret(&alice(), "gh", "casey").unwrap();
}

/// A use (ADR-052, step 2): judged by the rules at HEAD; the agent's first use in each session on the record once,
/// and each refusal of the agent's once; a person's own uses add nothing to it.
#[test]
fn a_use_is_judged_by_its_rule_and_the_agents_first_is_recorded() {
    use torq_broker::secrets::Use;
    let m = Machine::new();
    let repo = m.b.repo.root().to_path_buf();
    fs::create_dir_all(repo.join("users/alice")).unwrap();
    fs::write(
        repo.join("users/alice/secrets.json"),
        r#"{"gh":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"],"approve":"never"},"own":{"kind":"token","https":{"hosts":["api.github.com"]}}}"#,
    )
    .unwrap();
    let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().unwrap().success());
    git(&["add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "rules"]);
    m.b.set_secret(&alice(), "gh", VALUE, "alice").unwrap();
    m.b.set_secret(&alice(), "own", "alice's own", "alice").unwrap();
    let session = torq_broker::session::start(&m.b).unwrap().id;
    let agent = |name: &str, host: &str, session: Option<&str>| Use { owner: alice(), name: name.into(), host: host.into(), actor: "torq-agent".into(), session: session.map(String::from), ssh: false };
    let count = |kind: &str| m.ledger().matches(&format!("\"kind\":\"{kind}\"")).count();
    assert_eq!(m.b.use_secret(&agent("gh", "api.github.com", Some(&session))).unwrap().value, VALUE);
    assert_eq!(m.b.use_secret(&agent("gh", "api.github.com", Some(&session))).unwrap().value, VALUE);
    assert_eq!(count("secret_first_use"), 1, "the first use in a session, once");
    let other = torq_broker::session::start(&m.b).unwrap().id;
    m.b.use_secret(&agent("gh", "api.github.com", Some(&other))).unwrap();
    assert_eq!(count("secret_first_use"), 2, "and once in each session");
    for _ in 0..3 {
        assert!(m.b.use_secret(&agent("gh", "evil.example.com", Some(&session))).is_err());
        assert!(m.b.use_secret(&agent("own", "api.github.com", Some(&session))).is_err());
    }
    for i in 0..5 {
        assert!(m.b.use_secret(&agent(&format!("made-up-{i}"), "api.github.com", Some(&session))).is_err());
    }
    assert_eq!(count("secret_use_refused"), 2, "each refusal once, not each time; a name with no rule, never: the daemon's log has it");
    torq_broker::session::taint(&m.b, &session, "web").unwrap();
    assert!(m.b.use_secret(&agent("gh", "api.github.com", Some(&session))).is_err(), "tainted: a person approves, not built yet");
    assert!(m.b.use_secret(&agent("gh", "api.github.com", None)).is_err(), "no session is a tainted one");
    assert!(m.b.use_secret(&agent("gh", "api.github.com", Some("0123456789abcdef0123456789abcdef"))).is_err(), "no such session");
    let before = m.ledger();
    let own = Use { owner: alice(), name: "own".into(), host: "api.github.com".into(), actor: "alice".into(), session: None, ssh: false };
    let g = m.b.use_secret(&own).unwrap();
    assert_eq!((g.value.as_str(), g.header.as_str(), g.prefix.as_str()), ("alice's own", "Authorization", "Bearer "));
    assert_eq!(m.ledger(), before, "a person's own use: the daemon's log, not the ledger");
    assert!(m.b.use_secret(&Use { session: Some(session.clone()), ..own }).is_err(), "a person names no session");
    assert!(!m.ledger().contains(VALUE));
}

/// An agent socket's grant (ADR-052, SSH): who it is for, for an hour; a session only the agent's, and one the
/// broker holds.
#[test]
fn an_agent_sockets_grant_is_who_asked() {
    let m = Machine::new();
    let g = m.b.grant_agent("alice", 1000, None).unwrap();
    let read = m.b.grant(&g).unwrap();
    assert_eq!((read.actor.as_str(), read.uid, read.session), ("alice", 1000, None));
    assert!(m.b.grant_agent("alice", 1000, Some("0123456789abcdef0123456789abcdef")).is_err(), "a person names no session");
    assert!(m.b.grant_agent("torq-agent", 990, Some("0123456789abcdef0123456789abcdef")).is_err(), "no such session");
    let s = torq_broker::session::start(&m.b).unwrap().id;
    let a = m.b.grant_agent("torq-agent", 990, Some(&s)).unwrap();
    assert_eq!(m.b.grant(&a).unwrap().session.as_deref(), Some(s.as_str()));
    for bad in ["", "../x", "0123456789abcdef0123456789abcdeg", "00000000000000000000000000000000"] {
        assert!(m.b.grant(bad).is_err(), "{bad:?}");
    }
    assert!(m.b.ssh_keys_for("alice").is_empty(), "no keys yet");
}

/// An SSH key's public half, read from its own header (no program given the private key, no file of it), beside it;
/// a key under a passphrase, or another form, refused saying why, and the old one left listed.
#[test]
fn an_ssh_keys_public_half_is_kept_beside_it() {
    const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\nQyNTUxOQAAACALEJlaQgiCLSgzMKjXIAECACR/bjD9FbMY71yZLrLm3AAAAIjyZMAb8mTA\n";
    let m = Machine::new();
    let Some(key) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../wire/src/secrets.rs")).ok().and_then(|w| {
        let start = w.find("const TEST_KEY: &str = \"")? + "const TEST_KEY: &str = \"".len();
        Some(w[start..start + w[start..].find('"')?].replace("\\n", "\n"))
    }) else {
        panic!("the test key")
    };
    assert!(key.starts_with(KEY), "the same key as torq-wire's test");
    m.b.set_secret(&alice(), "deploy", &key, "alice").unwrap();
    let public = fs::read_to_string(m.store().join("users/alice/deploy.pub")).unwrap();
    assert_eq!(public.trim(), "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAsQmVpCCIItKDMwqNcgAQIAJH9uMP0VsxjvXJkusubc");
    let left: Vec<String> = fs::read_dir(m.store().join("users/alice")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(left.iter().all(|n| n == "deploy" || n == "deploy.pub"), "nothing else, no key file of any kind: {left:?}");
    let e = format!("{:#}", m.b.set_secret(&alice(), "deploy", "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----", "alice").unwrap_err());
    assert!(e.contains("PEM"), "{e}");
    assert_eq!(fs::read_to_string(m.store().join("users/alice/deploy.pub")).unwrap(), public, "the old one still listed");
    m.b.set_secret(&alice(), "deploy", "now a token", "alice").unwrap();
    assert!(!m.store().join("users/alice/deploy.pub").exists(), "not a key now: no public half");
}
