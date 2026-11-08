//! The agent's sessions, and a person's trust (ADR-042, 6 and 7, amended).
//!
//! **Sessions.** A program of the agent's (`torq-doctor`) starts a session at the broker over its
//! socket, as `torq-agent`; the broker makes its record here, in its own state directory (root's,
//! 0600). Each read of untrusted text the program does, it reports (`session-taint`) before the model
//! is shown the text; the broker marks the record tainted — sticky: nothing clears it, there is no
//! command that does. A proposal of the agent's names its session: it is tainted as this record says,
//! the caller's `--tainted` only adding to it; one that names none is tainted. The ledger holds a
//! session only where a proposal cites it: the proposal's entry carries the session's id, when it
//! started and what tainted it — so a session that proposed nothing costs the ledger nothing, and the
//! record of one that did outlives its file here.
//!
//! What the broker can vouch for is what was reported to it: it cannot see what a program of the
//! agent's reads. So a record is the agent's own word, kept where the agent cannot change it — a
//! report cannot be taken back, but a program that never reports (any tool run as the agent other than
//! TorqOS's own) has an untainted session (KNOWN-GAPS 6zq). Hence the default: no session, tainted.
//!
//! **Trust.** A person grants, with their password, "for the next N minutes (at most 60), the agent's
//! user-tier proposals in my own folder apply without asking": `trust_granted`, the ledger the only
//! record of it. While it holds, a proposal of the agent's that names a session, is not tainted, is
//! wholly in that person's folder (user tier, and on the agent's list) and could not cut the machine
//! off is applied as soon as it is made, as that person, its `applied` entry marked with the grant —
//! each its own change, revertible. Never a system-tier change, never a tainted one. Grants, revokes
//! and expiries are written under the sessions' own short lock, never the broker's: a revoke is on the
//! record at once, even while a trusted change is being built, and every trusted apply checks the
//! record again, under the broker's lock, just before it starts. Its time ends it (`trust_expired`,
//! ledgered by the next operation that writes trust or may apply under it — it is over at its time
//! whether or not that has been written).

use crate::ledger::{Entry, NewEntry};
use crate::policy::{self, Actor, Tier};
use crate::{socket, Broker, Lock};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{Read, Write};
use std::path::PathBuf;

/// How many sessions may be started in an hour: past that, refused. One account is the agent, so the
/// doctor's sessions and any other agent program's share it: a program run as the agent can use it up
/// and keep the doctor from starting for the rest of the hour — refused, never less careful.
pub const SESSIONS_AN_HOUR: usize = 600;
/// How many session records are kept: past that, the oldest go. A proposal naming a session whose
/// record is gone is refused, never taken as untainted; what tainted it is in its proposal's entry.
pub const SESSIONS_KEPT: usize = 2000;
/// The longest a person's trust lasts.
pub const TRUST_MAX_MINUTES: u32 = 60;

/// What the broker holds of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub started: String,
    pub tainted: bool,
    /// What tainted it, as reported: a tool's name.
    pub tainted_by: Vec<String>,
    /// The proposals that cited it, as the broker ledgered them (what the agent is told it did).
    #[serde(default)]
    pub proposals: Vec<Cited>,
}

/// A proposal that cited a session, and the change it became if a person's trust applied it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cited {
    pub proposal: u64,
    #[serde(default)]
    pub applied: Option<u64>,
}

/// A session's id: 128 random bits, as 32 lowercase hex digits.
pub fn id_ok(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a taint's source may be called: a tool's name.
fn source_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn dir(b: &Broker) -> PathBuf {
    b.state_dir.join("sessions")
}

/// The record of session `id`, if there is one (never through a link).
pub fn read(b: &Broker, id: &str) -> Result<Option<Record>> {
    use std::os::unix::fs::OpenOptionsExt;
    if !id_ok(id) {
        bail!("{id:?} is not a session's id");
    }
    let p = dir(b).join(format!("{id}.json"));
    let f = match std::fs::OpenOptions::new().read(true).custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32).open(&p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    let mut s = String::new();
    f.take(64 * 1024).read_to_string(&mut s)?;
    Ok(Some(serde_json::from_str(&s).with_context(|| format!("reading {}", p.display()))?))
}

/// Write a record: a new file beside it, then renamed over it — a record is whole or as it was.
fn write(b: &Broker, r: &Record) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let d = dir(b);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d)?;
    let part = d.join(format!(".{}.new", r.id));
    let _ = std::fs::remove_file(&part);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
    f.write_all(&serde_json::to_vec(r)?)?;
    f.sync_all()?;
    std::fs::rename(&part, d.join(format!("{}.json", r.id)))?;
    std::fs::File::open(&d)?.sync_all()?;
    Ok(())
}

/// The sessions' and trust's own lock: short, never the broker's (which a rebuild holds for
/// minutes), so that a report or a revoke is never kept waiting by a change. Never taken twice in
/// one process at once; may be taken while the broker's is held, never the other way round.
fn lock(b: &Broker) -> Result<Lock> {
    Lock::acquire_named(&b.state_dir, "sessions.lock", 10)
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Every record kept, as (when it started, its id).
fn kept(b: &Broker) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir(b)) else { return Ok(out) };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json").filter(|i| id_ok(i)) else { continue };
        if let Some(r) = read(b, id)? {
            out.push((r.started, r.id));
        }
    }
    out.sort();
    Ok(out)
}

/// Start a session of the agent's: its record, here. At most `SESSIONS_AN_HOUR` an hour; past
/// `SESSIONS_KEPT`, the oldest records go.
pub fn start(b: &Broker) -> Result<Record> {
    let _l = lock(b)?;
    let kept = kept(b)?;
    let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
    let recent = kept.iter().filter(|(t, _)| chrono::DateTime::parse_from_rfc3339(t).is_ok_and(|t| t > hour_ago)).count();
    if recent >= SESSIONS_AN_HOUR {
        bail!("refused: {recent} of the agent's sessions were started in the last hour (at most {SESSIONS_AN_HOUR})");
    }
    for (_, id) in kept.iter().take((kept.len() + 1).saturating_sub(SESSIONS_KEPT)) {
        std::fs::remove_file(dir(b).join(format!("{id}.json")))?;
    }
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).context("reading /dev/urandom")?;
    let id: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
    if read(b, &id)?.is_some() {
        bail!("a session {id} exists already");
    }
    let r = Record { id, started: now(), tainted: false, tainted_by: vec![], proposals: vec![] };
    write(b, &r)?;
    Ok(r)
}

/// How many distinct sources a session's record names; past them it is tainted all the same.
pub const SOURCES_KEPT: usize = 32;

/// Session `id` read untrusted text (from `source`): tainted, for good.
pub fn taint(b: &Broker, id: &str, source: &str) -> Result<Record> {
    if !source_ok(source) {
        bail!("{source:?} is not a source's name (a tool's: letters, digits, - and _)");
    }
    let _l = lock(b)?;
    let mut r = read(b, id)?.context("refused: no such session")?;
    // Bounded: a record any agent program can report into must not grow until no one can start a
    // session (the audit of 0.3.0). Tainted is tainted; past the bound, what tainted it is said as "more".
    if !r.tainted_by.iter().any(|s| s == source) && r.tainted_by.len() < SOURCES_KEPT {
        r.tainted_by.push(source.into());
    }
    r.tainted = true;
    write(b, &r)?;
    Ok(r)
}

/// Whether a proposal is tainted — by `actor`, the caller's `flag` (which only adds), and the
/// broker's record of `session` — and what of the session goes into its entry. A person's proposal
/// names no session; an agent's that names none is tainted; one that names a session not held here is
/// refused.
pub fn judge(b: &Broker, actor: Actor, flag: bool, session: Option<&str>) -> Result<(bool, Option<serde_json::Value>)> {
    match (actor, session) {
        (Actor::User, Some(_)) => bail!("refused: a session is the agent's; a person's proposal names none"),
        (Actor::User, None) => Ok((flag, None)),
        (Actor::Agent, None) => Ok((true, None)),
        (Actor::Agent, Some(id)) => {
            let r = read(b, id)?.context("refused: the proposal names no session the broker holds")?;
            Ok((flag || r.tainted, Some(json!({ "id": r.id, "started": r.started, "tainted_by": r.tainted_by }))))
        }
    }
}

/// The proposal `seq` cited session `id` (and, `applied`, became that change under trust): noted in
/// its record, for what the agent is told.
pub fn cite(b: &Broker, id: &str, seq: u64, applied: Option<u64>) -> Result<()> {
    let _l = lock(b)?;
    let Some(mut r) = read(b, id)? else { return Ok(()) };
    match r.proposals.iter_mut().find(|c| c.proposal == seq) {
        Some(c) => c.applied = applied.or(c.applied),
        None => r.proposals.push(Cited { proposal: seq, applied }),
    }
    write(b, &r)
}

/// A person's trust, granted and not yet revoked or expired on the record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Grant {
    /// Its `trust_granted` entry.
    pub seq: u64,
    pub person: String,
    pub minutes: u64,
    pub until: String,
    /// When it was granted: its entry's time. A grant lasts no longer than its minutes from then,
    /// whatever its `until` says (a ledger from elsewhere is someone else's text: the audit of 0.3.0).
    pub at: String,
}

impl Grant {
    fn over(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        let minutes = i64::try_from(self.minutes.min(u64::from(TRUST_MAX_MINUTES))).unwrap_or(0);
        let ends = chrono::DateTime::parse_from_rfc3339(&self.at).map(|t| t + chrono::Duration::minutes(minutes));
        let until = chrono::DateTime::parse_from_rfc3339(&self.until);
        match (until, ends) {
            (Ok(u), Ok(e)) => u.min(e) <= at,
            _ => true,
        }
    }
}

/// Every grant not settled on the record (revoked or expired), oldest first.
pub fn unsettled(entries: &[Entry]) -> Vec<Grant> {
    let settled = |seq: u64| entries.iter().any(|e| matches!(e.body.kind.as_str(), "trust_revoked" | "trust_expired") && e.body.data["grant"] == json!(seq));
    entries
        .iter()
        .filter(|e| e.body.kind == "trust_granted" && !settled(e.body.seq))
        .filter_map(|e| {
            Some(Grant {
                seq: e.body.seq,
                person: e.body.data["person"].as_str()?.into(),
                minutes: e.body.data["minutes"].as_u64()?,
                until: e.body.data["until"].as_str()?.into(),
                at: e.body.ts.clone(),
            })
        })
        .collect()
}

/// The grants that hold now: unsettled, and their time not over.
pub fn active(entries: &[Entry]) -> Vec<Grant> {
    let now = chrono::Utc::now();
    unsettled(entries).into_iter().filter(|g| !g.over(now)).collect()
}

/// Ledger `trust_expired` for every grant whose time is over and is not yet settled, under the
/// sessions' lock (held).
fn settle_locked(b: &Broker) -> Result<()> {
    let now = chrono::Utc::now();
    for g in unsettled(&b.ledger.read_all()?).into_iter().filter(|g| g.over(now)) {
        b.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: None,
            kind: "trust_expired".into(),
            tier: Tier::User.to_string(),
            tainted: false,
            reason: format!("{}'s trust in the agent ended at {}", g.person, g.until),
            data: json!({ "grant": g.seq, "person": g.person, "until": g.until }),
        })?;
    }
    Ok(())
}

/// `settle_locked`, taking the sessions' lock: by every operation that may apply under trust.
pub fn settle_trust(b: &Broker) -> Result<()> {
    let _l = lock(b)?;
    settle_locked(b)
}

/// `person` trusts the agent's user-tier proposals in their own folder for `minutes`. (Over the
/// socket: only that person, with their password.)
pub fn grant(b: &Broker, person: &str, minutes: u32) -> Result<Entry> {
    if !policy::user_name_ok(person) {
        bail!("{person:?} is not a person's name");
    }
    if !(1..=TRUST_MAX_MINUTES).contains(&minutes) {
        bail!("refused: trust lasts 1 to {TRUST_MAX_MINUTES} minutes, not {minutes}");
    }
    if !b.agent_limits.users.iter().any(|u| u == person) {
        bail!("refused: the agent may not propose in {person}'s folder (torq.agent.mayPropose.users), so there is nothing to trust it with");
    }
    let _l = lock(b)?;
    settle_locked(b)?;
    if let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) {
        bail!("refused: {person} trusts the agent already, until {} (#{}): revoke it first", g.until, g.seq);
    }
    let until = (chrono::Utc::now() + chrono::Duration::minutes(minutes.into())).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    b.ledger.append(NewEntry {
        actor: Actor::User.to_string(),
        approved_by: Some(person.into()),
        kind: "trust_granted".into(),
        tier: Tier::User.to_string(),
        tainted: false,
        reason: format!("{person} lets the agent's proposals in their own folder apply without asking, for {minutes} minutes"),
        data: json!({ "person": person, "minutes": minutes, "until": until }),
    })
}

/// End `person`'s trust now, if they have granted one: on the record at once, whatever the broker is
/// doing (the sessions' lock, never the broker's). A trusted apply already started finishes; none
/// starts after.
pub fn revoke(b: &Broker, person: &str) -> Result<Option<Entry>> {
    let _l = lock(b)?;
    settle_locked(b)?;
    let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) else { return Ok(None) };
    Ok(Some(b.ledger.append(NewEntry {
        actor: Actor::User.to_string(),
        approved_by: Some(person.into()),
        kind: "trust_revoked".into(),
        tier: Tier::User.to_string(),
        tainted: false,
        reason: format!("{person} revoked their trust in the agent"),
        data: json!({ "grant": g.seq, "person": person }),
    })?))
}

/// What of proposal `p` could cut the machine off, if anything (`cuts_off`, recorded when proposed).
pub fn cuts_off(p: &Entry) -> Option<Vec<String>> {
    let c: Vec<String> = p.body.data["cuts_off"].as_array()?.iter().filter_map(|s| s.as_str().map(String::from)).collect();
    (!c.is_empty()).then_some(c)
}

/// The session a proposal cites, by its id (`data.session.id`).
pub fn cited(p: &Entry) -> Option<&str> {
    p.body.data["session"]["id"].as_str()
}

/// Whose trust `p` could be applied under: an agent's proposal, naming a session, not tainted, user
/// tier, wholly one person's folder, nothing that could cut the machine off. None: not one.
fn trusted_by(p: &Entry) -> Option<String> {
    let b = &p.body;
    if b.kind != "proposal" || b.actor != Actor::Agent.to_string() || b.tainted || b.tier != Tier::User.to_string() || cited(p).is_none() || cuts_off(p).is_some() {
        return None;
    }
    let files: Vec<String> = b.data["files"].as_array()?.iter().filter_map(|f| f.as_str().map(String::from)).collect();
    let person = files.first()?.strip_prefix("users/")?.split_once('/')?.0.to_string();
    socket::theirs(&person, &socket::Change { tier: Tier::User, files }).then_some(person)
}

/// Apply proposal `p` now if a person's trust covers it (`trusted_by`, and their grant holding): as
/// that person, its entry marked with the grant. None: not covered; it waits for a person. Judged
/// under the broker's lock, with the sessions' lock held only while the record is read — so a revoke
/// is never kept waiting by the build.
pub fn apply_trusted(b: &Broker, p: &Entry) -> Result<Option<Entry>> {
    if trusted_by(p).is_none() {
        return Ok(None);
    }
    let lock = Lock::acquire(&b.state_dir)?;
    let (p, person, grant) = {
        let _s = self::lock(b)?;
        settle_locked(b)?;
        // The proposal as the ledger has it, and its session as the broker holds it now: tainted since
        // the proposal, it is not trusted; its grant revoked or over, nor is it.
        let p = b.ledger.get(p.body.seq)?;
        let Some(person) = trusted_by(&p) else { return Ok(None) };
        if cited(&p).map(|s| read(b, s)).transpose()?.flatten().is_none_or(|r| r.tainted) {
            return Ok(None);
        }
        let Some(g) = active(&b.ledger.read_all()?).into_iter().find(|g| g.person == person) else { return Ok(None) };
        (p, person, g.seq)
    };
    let e = b.apply_locked(&lock, p.body.seq, &person, false, None, Some(("trust", grant)))?;
    if let Some(id) = cited(&p) {
        cite(b, id, p.body.seq, Some(e.body.seq))?;
    }
    Ok(Some(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grant lasts its minutes from when it was made, whatever its `until` says: a ledger from
    /// elsewhere is someone else's text (the audit of 0.3.0).
    #[test]
    fn a_grant_lasts_its_minutes_whatever_its_until_says() {
        let g = |at: &str, minutes: u64, until: &str| Grant { seq: 1, person: "casey".into(), minutes, until: until.into(), at: at.into() };
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-08T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert!(g("2026-11-08T10:00:00Z", 60, "2099-01-01T00:00:00Z").over(now), "60 minutes from 10:00 is over at 12:00");
        assert!(!g("2026-11-08T11:30:00Z", 60, "2099-01-01T00:00:00Z").over(now));
        assert!(g("2026-11-08T11:30:00Z", 6000, "2099-01-01T00:00:00Z").over(now + chrono::Duration::minutes(61)), "never more than the most a grant may be");
        assert!(g("2026-11-08T11:30:00Z", 60, "2026-11-08T11:45:00Z").over(now), "nor past its until");
        assert!(g("not a time", 60, "2099-01-01T00:00:00Z").over(now), "unreadable is over");
    }
}
