//! The agent's sessions, held by the broker, and a person's trust (ADR-042, amended; session.rs).
//! Each test fails if its guard is taken out.

#![forbid(unsafe_code)]
#![forbid(dead_code)]

use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use torq_broker::ledger::NewEntry;
use torq_broker::{agent::Limits, policy::Actor, session, Broker};

fn setup(settings: &[&str]) -> (tempfile::TempDir, Broker) {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("config");
    fs::create_dir_all(repo.join("users")).unwrap();
    fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
    let limits = Limits { users: vec!["casey".into()], settings: settings.iter().map(|s| s.to_string()).collect() };
    let b = Broker::new(&repo, dir.path().join("state"), "true").with_agent_limits(&limits);
    b.init().unwrap();
    (dir, b)
}

fn diff(dir: &Path, name: &str, path: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+{body}\n")).unwrap();
    p
}

fn htop(dir: &Path) -> PathBuf {
    diff(dir, "htop.diff", "users/casey/packages.json", "[\"htop\"]")
}

fn propose(b: &Broker, d: &Path, actor: Actor, tainted: bool, session: Option<&str>) -> anyhow::Result<torq_broker::ledger::Entry> {
    b.propose_in(Some(&b.repo.head().unwrap()), d, "install htop", actor, tainted, session)
}

fn kinds(b: &Broker, kind: &str) -> usize {
    b.ledger.read_all().unwrap().iter().filter(|e| e.body.kind == kind).count()
}

#[test]
fn an_agents_proposal_without_a_session_is_tainted() {
    let (dir, b) = setup(&[]);
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, None).unwrap();
    assert!(p.body.tainted, "no session: tainted, whatever the caller says");
    assert!(b.apply(p.body.seq, "casey", false).is_err(), "a tainted proposal needs --confirm-tainted");
    // A person's proposal is as they say.
    let p = propose(&b, &diff(dir.path(), "p.diff", "users/sam/packages.json", "[\"jq\"]"), Actor::User, false, None).unwrap();
    assert!(!p.body.tainted);
}

#[test]
fn the_broker_holds_the_taint_and_nothing_clears_it() {
    let (dir, b) = setup(&[]);
    let before = b.ledger.read_all().unwrap().len();
    let s = session::start(&b).unwrap();
    // Its record is the broker's, in its state directory, private; the ledger has nothing of it yet.
    use std::os::unix::fs::PermissionsExt;
    let rec = dir.path().join("state/sessions").join(format!("{}.json", s.id));
    assert_eq!(fs::metadata(&rec).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(b.ledger.read_all().unwrap().len(), before, "a session that proposes nothing costs the ledger nothing");
    // A clean session's proposal is not tainted; the caller's --tainted still adds.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(!p.body.tainted);
    assert_eq!(p.body.data["session"], json!({ "id": s.id, "started": s.started, "tainted_by": [] }));
    let d2 = diff(dir.path(), "two.diff", "users/casey/packages.json", "[\"jq\"]");
    assert!(propose(&b, &d2, Actor::Agent, true, Some(&s.id)).unwrap().body.tainted, "the caller's flag adds");
    // Tainted, sticky; a proposal saying otherwise is tainted all the same, and its entry says by what.
    session::taint(&b, &s.id, "read_journal").unwrap();
    session::taint(&b, &s.id, "show_ledger").unwrap();
    let r = session::read(&b, &s.id).unwrap().unwrap();
    assert!(r.tainted && r.tainted_by == ["read_journal", "show_ledger"]);
    let d3 = diff(dir.path(), "three.diff", "users/casey/packages.json", "[\"tree\"]");
    let p3 = propose(&b, &d3, Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(p3.body.tainted, "from the broker's record, not the caller's word");
    assert_eq!(p3.body.data["session"]["tainted_by"], json!(["read_journal", "show_ledger"]), "the ledger keeps what tainted it");
    // The record says what cited it, for what the agent is told.
    let cited: Vec<u64> = session::read(&b, &s.id).unwrap().unwrap().proposals.iter().map(|c| c.proposal).collect();
    assert_eq!(cited.last(), Some(&p3.body.seq));
    // A source that is not a tool's name is refused, and nothing changes.
    assert!(session::taint(&b, &s.id, "x; rm").is_err());
}

#[test]
fn a_session_must_be_the_brokers_and_the_agents() {
    let (dir, b) = setup(&[]);
    let d = htop(dir.path());
    let err = format!("{:#}", propose(&b, &d, Actor::Agent, false, Some(&"0".repeat(32))).unwrap_err());
    assert!(err.contains("no session the broker holds"), "{err}");
    assert!(kinds(&b, "refused") == 1, "an agent's refusal is ledgered");
    for bad in ["../../x", &"A".repeat(32), &"0".repeat(31)] {
        let err = format!("{:#}", propose(&b, &d, Actor::Agent, false, Some(bad)).unwrap_err());
        assert!(err.contains("is not a session's id"), "{bad}: {err}");
        assert!(session::read(&b, bad).is_err(), "{bad}");
    }
    let s = session::start(&b).unwrap();
    assert!(propose(&b, &d, Actor::User, false, Some(&s.id)).is_err(), "a person names no session");
    assert!(session::taint(&b, &"1".repeat(32), "read_journal").is_err(), "no such session");
}

/// The review of w5: sessions are bounded in what they keep, and an old one's file going makes a
/// proposal naming it refused, never untainted.
#[test]
fn sessions_are_bounded() {
    let (dir, b) = setup(&[]);
    let sessions = dir.path().join("state/sessions");
    fs::create_dir_all(&sessions).unwrap();
    // As many as are kept, from long ago: one more start takes the oldest away.
    for i in 0..session::SESSIONS_KEPT {
        let id = format!("{i:032x}");
        fs::write(sessions.join(format!("{id}.json")), json!({"id": id, "started": format!("2020-01-01T00:{:02}:{:02}Z", i / 60 % 60, i % 60), "tainted": i == 0, "tainted_by": []}).to_string()).unwrap();
    }
    let oldest = format!("{:032x}", 0);
    session::start(&b).unwrap();
    assert_eq!(fs::read_dir(&sessions).unwrap().count(), session::SESSIONS_KEPT);
    assert!(session::read(&b, &oldest).unwrap().is_none(), "the oldest went");
    assert!(propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&oldest)).is_err(), "gone: refused, not untainted");
    // At most so many started in an hour.
    let now = chrono::Utc::now().to_rfc3339();
    for i in 0..session::SESSIONS_AN_HOUR {
        let id = format!("{:032x}", 1_000_000 + i);
        fs::write(sessions.join(format!("{id}.json")), json!({"id": id, "started": now, "tainted": false, "tainted_by": []}).to_string()).unwrap();
    }
    assert!(format!("{:#}", session::start(&b).unwrap_err()).contains("in the last hour"));
}

#[test]
fn under_trust_a_clean_proposal_in_the_persons_folder_applies() {
    let (dir, b) = setup(&[]);
    let g = session::grant(&b, "casey", 30).unwrap();
    assert_eq!((g.body.kind.as_str(), g.body.approved_by.as_deref()), ("trust_granted", Some("casey")));
    assert_eq!(session::active(&b.ledger.read_all().unwrap()).len(), 1);
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    let a = session::apply_trusted(&b, &p).unwrap().expect("applied under trust");
    assert_eq!(a.body.kind, "applied");
    assert_eq!(a.body.approved_by.as_deref(), Some("casey"));
    assert_eq!(a.body.actor, "agent");
    assert_eq!(a.body.data["trust"], json!(g.body.seq), "the apply names the grant it was made under");
    // Revertible as any change.
    b.revert(a.body.seq, "casey").unwrap();
}

/// ADR-053 (f): trust's automatic apply takes the same re-check as a person's. The configuration moved on
/// in another file: applied, re-checked; the same file made since, so the diff would do something else:
/// refused, nothing applied.
#[test]
fn under_trust_a_moved_configuration_is_rechecked() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    let night = dir.path().join("night.diff");
    fs::write(&night, "--- a/desktop.nix\n+++ b/desktop.nix\n@@ -1 +1 @@\n-{ }\n+{ nightLight = true; }\n").unwrap();
    let o = b.propose(&night, "night", Actor::User, false).unwrap();
    b.apply(o.body.seq, "casey", false).unwrap();
    let moved = b.repo.head().unwrap();
    let a = session::apply_trusted(&b, &p).unwrap().expect("applied under trust");
    assert_eq!(a.body.data["rechecked_on"], json!(moved));

    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let q = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    // The same file, made by the person meanwhile: the agent's diff, which creates it, would now do
    // something else (here: nothing at all, it no longer applies).
    let mine = diff(dir.path(), "mine.diff", "users/casey/packages.json", "[\"git\"]");
    let m = b.propose(&mine, "mine", Actor::User, false).unwrap();
    b.apply(m.body.seq, "casey", false).unwrap();
    let err = format!("{:#}", session::apply_trusted(&b, &q).unwrap_err());
    assert!(err.contains("would now do something different"), "{err}");
    assert_eq!(fs::read_to_string(b.repo.root().join("users/casey/packages.json")).unwrap(), "[\"git\"]\n");
}

#[test]
fn trust_never_covers_a_tainted_or_sessionless_or_system_proposal() {
    let (dir, b) = setup(&["services.openssh", "networking"]);
    session::grant(&b, "casey", 30).unwrap();
    // No session: tainted, waits for a person.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, None).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A tainted session.
    let s = session::start(&b).unwrap();
    session::taint(&b, &s.id, "read_journal").unwrap();
    let p = propose(&b, &diff(dir.path(), "b.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A clean session, but the proposal marked tainted by its caller: the mark counts.
    let clean = session::start(&b).unwrap();
    let p = propose(&b, &diff(dir.path(), "t.diff", "users/casey/packages.json", "[\"bat\"]"), Actor::Agent, true, Some(&clean.id)).unwrap();
    assert!(p.body.tainted && session::apply_trusted(&b, &p).unwrap().is_none());
    // System tier, from a clean session.
    let ssh = diff(dir.path(), "ssh.diff", "nixos/services.json", "{\"openssh\": {\"enable\": true}}");
    let p = propose(&b, &ssh, Actor::Agent, false, Some(&clean.id)).unwrap();
    assert_eq!(p.body.tier, "system");
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // A person's own proposal is theirs to apply: trust is for the agent's.
    let mine = propose(&b, &diff(dir.path(), "m.diff", "users/casey/packages.json", "[\"tree\"]"), Actor::User, false, None).unwrap();
    assert!(session::apply_trusted(&b, &mine).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 0);
}

#[test]
fn trust_is_the_persons_own_folder_only() {
    let (dir, b) = setup(&[]);
    let b = b.with_agent_limits(&Limits { users: vec!["casey".into(), "sam".into()], settings: vec![] });
    session::grant(&b, "sam", 30).unwrap();
    let s = session::start(&b).unwrap();
    // casey's folder: sam's trust does not cover it.
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    // Not on the agent's list: nothing to trust it with.
    assert!(format!("{:#}", session::grant(&b, "pat", 10).unwrap_err()).contains("mayPropose"));
}

#[test]
fn trust_is_bounded_revocable_and_expires_on_the_record() {
    let (dir, b) = setup(&[]);
    for bad in [0, 61] {
        assert!(session::grant(&b, "casey", bad).is_err(), "{bad}");
    }
    let g = session::grant(&b, "casey", 60).unwrap();
    assert!(session::grant(&b, "casey", 5).is_err(), "one at a time");
    let r = session::revoke(&b, "casey").unwrap().unwrap();
    assert_eq!((r.body.kind.as_str(), r.body.data["grant"].as_u64()), ("trust_revoked", Some(g.body.seq)));
    assert!(session::revoke(&b, "casey").unwrap().is_none());
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none(), "revoked");
    // A grant whose time is over: settled by the next operation, and covers nothing.
    let past = (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
    let old = b.ledger.append(NewEntry {
        actor: "user".into(),
        approved_by: Some("casey".into()),
        kind: "trust_granted".into(),
        tier: "user".into(),
        tainted: false,
        reason: "old".into(),
        data: json!({ "person": "casey", "minutes": 5, "until": past }),
    }).unwrap();
    assert!(session::active(&b.ledger.read_all().unwrap()).is_empty(), "over at its time, entry or not");
    let p2 = propose(&b, &diff(dir.path(), "j.diff", "users/casey/packages.json", "[\"jq\"]"), Actor::Agent, false, Some(&s.id)).unwrap();
    let expired = b.ledger.read_all().unwrap().into_iter().find(|e| e.body.kind == "trust_expired").expect("ledgered");
    assert_eq!(expired.body.data["grant"], json!(old.body.seq));
    assert!(session::apply_trusted(&b, &p2).unwrap().is_none());
}

/// ADR-042, 6: an agent's change that could lose the network is applied only as a commit confirmed.
#[test]
fn what_could_cut_the_machine_off_is_applied_confirmed() {
    let (dir, b) = setup(&["networking"]);
    let s = session::start(&b).unwrap();
    let wifi = diff(dir.path(), "w.diff", "nixos/networking.json", "{\"wireless\": {\"enable\": true}}");
    let p = propose(&b, &wifi, Actor::Agent, false, Some(&s.id)).unwrap();
    assert_eq!(p.body.data["cuts_off"], json!(["networking.wireless.enable"]));
    let err = format!("{:#}", b.apply(p.body.seq, "casey", false).unwrap_err());
    assert!(err.contains("commit confirmed"), "{err}");
    assert_eq!(kinds(&b, "applied"), 0);
    // A person's own change is theirs to make as they like.
    let mine = propose(&b, &diff(dir.path(), "m.diff", "nixos/time.json", "{\"timeZone\": \"UTC\"}"), Actor::User, false, None).unwrap();
    assert!(mine.body.data.get("cuts_off").is_none());
}

/// The review of w5: a session tainted after its proposal, before the trusted apply, is not trusted
/// (the record is read again, under the lock).
#[test]
fn a_session_tainted_before_the_apply_is_not_trusted() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let p = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(!p.body.tainted);
    session::taint(&b, &s.id, "read_journal").unwrap();
    assert!(session::apply_trusted(&b, &p).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 0);
}

/// The review of w5: a revoke never waits for the broker's lock — a trusted change being built
/// holds it — and no trusted apply starts after it.
#[test]
fn a_revoke_is_on_the_record_at_once_whatever_the_broker_is_doing() {
    let (dir, b) = setup(&[]);
    session::grant(&b, "casey", 30).unwrap();
    let s = session::start(&b).unwrap();
    let first = propose(&b, &htop(dir.path()), Actor::Agent, false, Some(&s.id)).unwrap();
    assert!(session::apply_trusted(&b, &first).unwrap().is_some(), "one applied under trust");
    let d = dir.path().join("r.diff");
    fs::write(&d, "--- a/users/casey/packages.json\n+++ b/users/casey/packages.json\n@@ -1 +1 @@\n-[\"htop\"]\n+[\"jq\"]\n").unwrap();
    let next = propose(&b, &d, Actor::Agent, false, Some(&s.id)).unwrap();
    // The broker's lock held, as a rebuild would: the revoke goes through, and quickly.
    let held = fs::File::open(dir.path().join("state/broker.lock")).unwrap();
    held.lock().unwrap();
    let t = std::time::Instant::now();
    let r = session::revoke(&b, "casey").unwrap().expect("revoked");
    assert!(t.elapsed() < std::time::Duration::from_secs(2), "it did not wait for the broker");
    assert_eq!(r.body.kind, "trust_revoked");
    drop(held);
    // The next apply checks the record again: revoked, nothing applied.
    assert!(session::apply_trusted(&b, &next).unwrap().is_none());
    assert_eq!(kinds(&b, "applied"), 1);
}

/// The review of w5: a commit at never takes what could cut the machine off — no one is there to confirm.
#[test]
fn a_commit_at_refuses_what_could_cut_the_machine_off() {
    let (dir, b) = setup(&["networking"]);
    let s = session::start(&b).unwrap();
    let wifi = diff(dir.path(), "w.diff", "nixos/networking.json", "{\"wireless\": {\"enable\": true}}");
    let p = propose(&b, &wifi, Actor::Agent, false, Some(&s.id)).unwrap();
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let err = format!("{:#}", torq_broker::at::Scheduler::new(&b, "true", "true").schedule(p.body.seq, "casey", &at).unwrap_err());
    assert!(err.contains("could lose the network or the screen"), "{err}");
    assert_eq!(kinds(&b, "apply_scheduled"), 0);
}
