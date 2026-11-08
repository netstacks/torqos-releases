//! The audit of 0.3.0: seams between sessions, trust, adopt and commit at. Each test was a finding:
//! it failed against the release candidate, and passes now.
#![forbid(unsafe_code)]
#![forbid(dead_code)]
use serde_json::json;
use std::fs;
use std::path::Path;
use torq_broker::ledger::NewEntry;
use torq_broker::{agent::Limits, policy::Actor, repo::Repo, session, Broker};

fn setup() -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
    let b = Broker::new(&repo, dir.path().join("state"), "true").with_agent_limits(&Limits { users: vec!["casey".into()], settings: vec![] });
    b.init().unwrap();
    (dir, b)
}


/// Any program run as the agent reports a thousand distinct "sources" on its own session: the record
/// passes the 64 KiB `read` takes, and from then on no session can be started, by anyone, ever.
#[test]
fn one_grown_record_stops_every_session() {
    let (_d, b) = setup();
    let s = session::start(&b).unwrap();
    for n in 0..1000 {
        session::taint(&b, &s.id, &format!("{n:0>60}")).unwrap();
    }
    let r = session::read(&b, &s.id).unwrap().unwrap();
    assert!(r.tainted && r.tainted_by.len() <= session::SOURCES_KEPT, "{}", r.tainted_by.len());
    assert!(session::start(&b).is_ok(), "a session can still be started");
}


/// Adopt's checks take a ledger that holds a standing grant and a hundred waiting schedules.
#[test]
fn adopt_takes_a_standing_grant_and_waiting_schedules() {
    let sh = |dir: &Path, script: &str| assert!(std::process::Command::new("sh").arg("-c").arg(script).current_dir(dir).status().unwrap().success(), "{script}");
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(&repo).unwrap();
    sh(&repo, "mkdir -p nixos system users && echo '{\"user\": \"alice\"}' > nixos/torq.json && echo '{\"host-name\": \"first\"}' > system/host.json \
        && echo '{\"\": \"hardware-configuration.nix\"}' > config-files.json && echo '{ }' > hardware-configuration.nix && echo '{ }' > desktop.nix");
    let b = Broker::new(&repo, dir.path().join("state"), "true");
    b.init().unwrap();
    let head = b.repo.head().unwrap();
    b.ledger.append(NewEntry { actor: "user".into(), approved_by: Some("alice".into()), kind: "trust_granted".into(), tier: "user".into(), tainted: false,
        reason: "x".into(), data: json!({ "person": "alice", "minutes": 60, "until": "2099-01-01T00:00:00Z" }) }).unwrap();
    for i in 0..100u64 {
        b.ledger.append(NewEntry { actor: "user".into(), approved_by: Some("alice".into()), kind: "apply_scheduled".into(), tier: "system".into(), tainted: false,
            reason: "x".into(), data: json!({ "proposal": 1, "at": "2099-01-01T00:00:00Z", "timer": i, "base": head, "diff_sha256": "0".repeat(64) }) }).unwrap();
    }
    let new_machine = Broker::new(dir.path().join("new-config"), dir.path().join("new-state"), "true");
    fs::create_dir_all(dir.path().join("new-state")).unwrap();
    let work = dir.path().join("work");
    let stage = dir.path().join("stage.git");
    sh(dir.path(), &format!("git clone -q {} {} && git clone -q --bare {} {}", repo.display(), work.display(), work.display(), stage.display()));
    sh(&stage, &format!("b=$(git hash-object -w {}) && t=$(printf '100644 blob %s\\tledger.jsonl\\n' $b | git mktree) && c=$(git -c user.name=t -c user.email=t@t commit-tree $t -m l) && git update-ref refs/torq/remote-ledger $c && git symbolic-ref HEAD refs/heads/main", b.ledger.path().display()));
    let r = new_machine.check_stage(&Repo::new(stage), "git@host:x.git");
    println!("check_stage: {:?}", r.as_ref().map(|a| a.entries).map_err(|e| format!("{e:#}")));
    assert!(r.is_err(), "BUG: adopt accepts a ledger with a standing trust grant and 100 waiting schedules");
}

/// What the agent's session puts in its proposal's entry, in the ledger itself: its whole tainted_by.
#[test]
fn a_session_pads_each_proposal_entry() {
    let (dir, b) = setup();
    let s = session::start(&b).unwrap();
    for n in 0..900 {
        session::taint(&b, &s.id, &format!("{n:0>60}")).unwrap();
    }
    let before = fs::metadata(b.ledger.path()).unwrap().len();
    let mut seqs = vec![];
    for i in 0..20 {
        let p = dir.path().join(format!("p{i}.diff"));
        fs::write(&p, format!("--- /dev/null\n+++ b/users/casey/packages.json\n@@ -0,0 +1 @@\n+[\"pkg{i}\"]\n")).unwrap();
        seqs.push(b.propose_in(Some(&b.repo.head().unwrap()), &p, "x", Actor::Agent, false, Some(&s.id)).unwrap().body.seq);
    }
    let grew = fs::metadata(b.ledger.path()).unwrap().len() - before;
    println!("20 proposals at one HEAD grew the ledger by {grew} bytes");
    assert!(grew < 200_000, "BUG: the agent's session padded the ledger by {grew} bytes in 20 proposals");
}

/// A lock padded past what a lock is was read as no lock at all, and nothing in it checked (A2).
#[test]
fn a_lock_too_large_to_read_is_refused_not_skipped() {
    let (dir, b) = setup();
    let copy = dir.path().join("copy");
    let sh = |d: &Path, s: &str| assert!(std::process::Command::new("sh").arg("-c").arg(s).current_dir(d).status().unwrap().success(), "{s}");
    sh(dir.path(), &format!("cp -r {} {}", b.repo.root().display(), copy.display()));
    fs::write(copy.join("flake.lock"), format!("{{\"pad\": \"{}\"}}\n", "A".repeat(1_200_000))).unwrap();
    sh(&copy, "git add -N flake.lock && git diff > ../big.diff");
    let p = b.propose_on(Some(&b.repo.head().unwrap()), &dir.path().join("big.diff"), "lock", Actor::User, false).unwrap();
    let e = format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(e.contains("larger than a lock is"), "{e}");
}
