//! `commit at` (ROADMAP M1.5): a proposal approved now and applied at a time, by a transient timer.
//!
//! `schedule` takes a proposal with a base — on a configuration moved on since, only one that still does
//! exactly what was shown (ADR-053) — approved by the
//! person now (over the socket, gated as `apply` is: their password for a system change), arms a
//! timer of its own (`{broker} run-scheduled {id}`), and ledgers `apply_scheduled`: the proposal, the
//! time, who, the timer, the base and the hash of the diff approved. At the time, `run-scheduled`
//! applies that proposal as that person — `applied` or `apply_failed`, marked `scheduled` — unless
//! a commit confirmed is waiting, the diff is not the one approved, or apply refuses it (on a
//! configuration moved on, one that would now do something other than was shown): then it is refused
//! and ledgered (`apply_refused`). `cancel-scheduled` ledgers `apply_cancelled` and stops the timer.
//!
//! What is scheduled is the ledger's word alone: a run finds its schedule there, by the timer's id,
//! under the broker lock, and a cancelled or settled one does nothing — so nothing outlives a cancel,
//! even a timer that could not be stopped. One at a time: a second would be prepared on the same
//! configuration the first changes, and land on one it was not prepared on; a person schedules the next
//! once the first has landed.

use crate::ledger::{Entry, NewEntry};
use crate::{pending_confirm, policy, Broker, Busy, Lock};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// The furthest ahead a commit may be scheduled.
pub const MAX_DAYS: i64 = 30;
/// How early a run may come (a clock stepped back a little): before that, it is not the time yet.
const EARLY_SECONDS: i64 = 60;
/// How late a run may come: after that (a machine asleep at its time, a clock stepped after boot, a
/// broker busy for long), it is refused on the record, never applied late.
pub const LATE_MINUTES: i64 = 15;

/// The timer: `{broker} run-scheduled {id}` at `{at}` (UTC, `YYYY-MM-DD HH:MM:SS UTC`). A wall-clock
/// timer, retried a bounded number of times while the broker is busy, as the rollback's is — for
/// longer than `LATE_MINUTES`, so that a run that keeps failing ends refused on the record.
pub const SCHEDULE: &str = "systemd-run --quiet --collect --unit=torq-commit-at-{id} --on-calendar={at} \
     --timer-property=AccuracySec=1s --property=Restart=on-failure --property=RestartSec=15 \
     --property=StartLimitIntervalSec=1800 --property=StartLimitBurst=100 \
     {broker} run-scheduled {id}";
/// Stop it: the timer only — its run, if already started, finds the cancel in the ledger.
pub const CANCEL: &str = "systemctl stop torq-commit-at-{id}.timer";

/// A commit scheduled and not yet settled: an `apply_scheduled` entry with no `applied`,
/// `apply_failed`, `apply_refused` or `apply_cancelled` after it naming it.
#[derive(Debug, Clone, Serialize)]
pub struct Scheduled {
    /// Its `apply_scheduled` entry: what `cancel-scheduled` takes.
    pub seq: u64,
    pub proposal: u64,
    pub at: String,
    pub approved_by: Option<String>,
    pub timer: u64,
    pub tier: String,
    pub reason: String,
    #[serde(skip)]
    diff_sha256: String,
    #[serde(skip)]
    boot_id: Option<String>,
}

/// Every commit scheduled and not settled, oldest first (the broker keeps it to one; a ledger from
/// elsewhere might hold more, and each is shown).
pub fn pending(entries: &[Entry]) -> Vec<Scheduled> {
    let settled = |seq: u64| {
        entries.iter().any(|e| {
            e.body.seq > seq
                && matches!(e.body.kind.as_str(), "applied" | "apply_failed" | "apply_refused" | "apply_cancelled")
                && e.body.data["scheduled"] == json!(seq)
        })
    };
    entries
        .iter()
        .filter(|e| e.body.kind == "apply_scheduled" && !settled(e.body.seq))
        .filter_map(|e| {
            let d = &e.body.data;
            Some(Scheduled {
                seq: e.body.seq,
                proposal: d["proposal"].as_u64()?,
                at: d["at"].as_str()?.into(),
                approved_by: e.body.approved_by.clone(),
                timer: d["timer"].as_u64()?,
                tier: e.body.tier.clone(),
                // The proposal's own reason: the entry's, less what it says of the time.
                reason: e.body.reason.strip_prefix(&format!("Scheduled for {}: ", d["at"].as_str()?)).unwrap_or(&e.body.reason).to_string(),
                diff_sha256: d["diff_sha256"].as_str()?.into(),
                boot_id: d["boot_id"].as_str().map(String::from),
            })
        })
        .collect()
}

/// `data`, marked as what applied it with no person there, if something did: the run of a schedule
/// (`("scheduled", <its entry>)`), or a person's trust (`("trust", <its grant>)`, session.rs).
pub fn note(mut data: serde_json::Value, by: Option<(&str, u64)>) -> serde_json::Value {
    if let Some((key, seq)) = by {
        data[key] = seq.into();
    }
    data
}

/// When a commit is to land: RFC 3339, in the future, at most `MAX_DAYS` ahead.
pub fn when(at: &str, now: chrono::DateTime<chrono::Utc>) -> Result<chrono::DateTime<chrono::Utc>> {
    let t = chrono::DateTime::parse_from_rfc3339(at).with_context(|| format!("{at:?} is not an RFC 3339 time"))?.with_timezone(&chrono::Utc);
    if t <= now {
        bail!("refused: {at} is not in the future");
    }
    if t > now + chrono::Duration::days(MAX_DAYS) {
        bail!("refused: {at} is more than {MAX_DAYS} days away");
    }
    Ok(t)
}

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The broker with the commands that arm and stop a schedule's timer (broker.json's `commitAt`,
/// `commitAtCancel`; tests', stubs).
pub struct Scheduler<'a> {
    b: &'a Broker,
    schedule: String,
    cancel: String,
    /// The machine's accounts, read at the run (who approved it must still be someone who may).
    passwd: std::path::PathBuf,
    group: std::path::PathBuf,
}

/// Whether a schedule's time `at` is more than `LATE_MINUTES` gone.
fn late(at: chrono::DateTime<chrono::Utc>) -> bool {
    chrono::Utc::now() > at + chrono::Duration::minutes(LATE_MINUTES)
}

fn time_of(s: &Scheduled) -> Result<chrono::DateTime<chrono::Utc>> {
    Ok(chrono::DateTime::parse_from_rfc3339(&s.at).with_context(|| format!("its time {:?} is not one", s.at))?.with_timezone(&chrono::Utc))
}

impl<'a> Scheduler<'a> {
    /// The broker, the commands that arm and stop a timer, and the machine's own accounts.
    pub fn new(b: &'a Broker, schedule: &str, cancel: &str) -> Self {
        Scheduler { b, schedule: schedule.into(), cancel: cancel.into(), passwd: "/etc/passwd".into(), group: "/etc/group".into() }
    }

    /// Other account files (tests').
    pub fn with_accounts(mut self, passwd: impl Into<std::path::PathBuf>, group: impl Into<std::path::PathBuf>) -> Self {
        (self.passwd, self.group) = (passwd.into(), group.into());
        self
    }

    fn arm(&self, lock: &Lock, timer: u64, at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        if self.schedule.is_empty() {
            bail!("this machine schedules no commits (no commitAt command in its broker settings)");
        }
        self.b.run_command(
            lock,
            "commit-at timer",
            &self.schedule,
            &[("id", &timer.to_string()), ("at", &at.format("%Y-%m-%d %H:%M:%S UTC").to_string()), ("broker", &self.b.exe.to_string_lossy())],
        )
    }

    /// The commits scheduled and not yet settled.
    pub fn scheduled(&self) -> Result<Vec<Scheduled>> {
        Ok(pending(&self.b.ledger.read_all()?))
    }

    /// Schedule proposal `proposal_seq` to be applied at `at`, approved now by `approved_by`.
    pub fn schedule(&self, proposal_seq: u64, approved_by: &str, at: &str) -> Result<Entry> {
        if !policy::user_name_ok(approved_by) {
            bail!("{approved_by:?} is not a person's name");
        }
        let at = when(at, chrono::Utc::now())?;
        let b = self.b;
        let lock = Lock::acquire(&b.state_dir)?;
        b.reconcile_locked(&lock, approved_by)?;
        b.refuse_unledgered_head()?;
        // One whose time is long gone without a run (its timer lost, or given up) settles here, on
        // the record: it blocks no one.
        for s in self.scheduled()? {
            if time_of(&s).map_or(true, late) {
                self.refuse(&s, "its time passed without it being applied")?;
            }
        }
        let entries = b.ledger.read_all()?;
        if let Some(s) = pending(&entries).first() {
            bail!(
                "refused: proposal #{} is already scheduled for {} (#{}); one at a time — cancel it first",
                s.proposal, s.at, s.seq
            );
        }
        let p = b.ledger.get(proposal_seq)?;
        if p.body.kind != "proposal" {
            bail!("entry {proposal_seq} is a '{}', not a proposal", p.body.kind);
        }
        if b.find_child("applied", "proposal", proposal_seq)?.is_some() {
            bail!("proposal {proposal_seq} was already applied");
        }
        // Read step by step, by the person, at the time: not something to leave to a timer.
        if p.body.tainted {
            bail!("refused: proposal {proposal_seq} came from a session that read untrusted content; apply it yourself");
        }
        // One that could cut the machine off is applied as a commit confirmed: no one is there at its time to confirm.
        if crate::session::cuts_off(&p).is_some() {
            bail!("refused: proposal {proposal_seq} could lose the network or the screen: it is applied confirmed, by a person who is there");
        }
        // Refused now, not at its time (where applying checks it again).
        crate::policy::check_secret_rules(p.body.data["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()), approved_by)?;
        let base = p.body.data["base"].as_str().context("refused: a scheduled change says the commit it was prepared against (its base)")?.to_string();
        let diff = b.proposal_text(&p, "diff")?;
        // On a configuration moved on from its base, only if it still does exactly what was shown
        // (ADR-053), as apply checks again at its time.
        let tmp = crate::DropFile(b.state_dir.join(format!("schedule-{proposal_seq}.diff")));
        std::fs::write(&tmp.0, &diff)?;
        let rechecked_on = b.recheck_on_head(&p, &tmp.0)?;
        drop(tmp);
        let timer = chrono::Utc::now().timestamp_millis().unsigned_abs();
        // The timer first, then the record: a timer whose record never got written finds no schedule
        // by its id and does nothing; a record without a timer would wait for ever.
        self.arm(&lock, timer, at)?;
        let at_s = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut data = json!({
            "proposal": proposal_seq,
            "at": at_s,
            "timer": timer,
            "base": base,
            "diff_sha256": sha(&diff),
            "boot_id": b.boot_id(),
        });
        if let Some(head) = rechecked_on {
            data["rechecked_on"] = head.into();
        }
        b.ledger.append(NewEntry {
            actor: p.body.actor.clone(),
            approved_by: Some(approved_by.into()),
            kind: "apply_scheduled".into(),
            tier: p.body.tier.clone(),
            tainted: false,
            reason: format!("Scheduled for {at_s}: {}", p.body.reason),
            data,
        })
    }

    /// The timer's run: apply the schedule its id names, as the person who approved it — or refuse,
    /// on the record. None: nothing is scheduled by that id (cancelled, settled, or never recorded).
    pub fn run(&self, timer: u64) -> Result<Option<Entry>> {
        let b = self.b;
        let lock = Lock::acquire(&b.state_dir)?;
        let Some(s) = self.scheduled()?.into_iter().find(|s| s.timer == timer) else {
            return Ok(None);
        };
        let at = match time_of(&s) {
            Ok(at) => at,
            Err(e) => return self.refuse(&s, &format!("{e:#}")).map(Some),
        };
        if chrono::Utc::now() < at - chrono::Duration::seconds(EARLY_SECONDS) {
            bail!("proposal #{} is scheduled for {}: not yet", s.proposal, s.at);
        }
        // Late — the machine asleep at its time, a clock stepped since, a broker busy all along — is
        // not applied: what was approved for one time is not approved for another.
        if late(at) {
            return self.refuse(&s, &format!("it is more than {LATE_MINUTES} minutes past its time")).map(Some);
        }
        // A switch outside the broker passes: the timer tries again (apply would refuse it, for good).
        if crate::switch_running() {
            bail!("a system switch ({}) is running outside the broker; the scheduled commit waits for it", crate::SWITCH_UNIT);
        }
        let Some(by) = s.approved_by.clone().filter(|n| policy::user_name_ok(n)) else {
            return self.refuse(&s, "it names no person who approved it").map(Some);
        };
        b.reconcile_locked(&lock, &by)?;
        if let Some(why) = self.refusal(&s)? {
            return self.refuse(&s, &why).map(Some);
        }
        match b.apply_locked(&lock, s.proposal, &by, false, None, Some(("scheduled", s.seq))) {
            Ok(e) => Ok(Some(e)),
            Err(e) if e.downcast_ref::<Busy>().is_some() => Err(e),
            // A refusal of apply's own (the proposal no longer does what was judged, say), or a
            // failure ledgered as `apply_failed`: settled either way, never tried again.
            Err(e) => {
                if self.scheduled()?.iter().any(|x| x.seq == s.seq) {
                    return self.refuse(&s, &format!("{e:#}")).map(Some);
                }
                Err(e)
            }
        }
    }

    /// Why schedule `s` must not be applied now, if it must not.
    fn refusal(&self, s: &Scheduled) -> Result<Option<String>> {
        let b = self.b;
        if let Some(pc) = pending_confirm(&b.ledger.read_all()?) {
            return Ok(Some(format!("change #{} (commit confirmed) was waiting for confirmation", pc.applied)));
        }
        // A configuration moved on since is apply's to judge, as for any apply (ADR-053): applied only if
        // the change does exactly what was shown, refused on the record otherwise.
        if b.find_child("applied", "proposal", s.proposal)?.is_some() {
            return Ok(Some(format!("proposal #{} was already applied", s.proposal)));
        }
        let p = b.ledger.get(s.proposal)?;
        if let Some(why) = self.approver_gone(s, &p)? {
            return Ok(Some(why));
        }
        // What is applied is what was approved: the diff by its hash, kept beside the ledger. The
        // proposal's entry is in the hash chain and its text is checked against it (proposal_text);
        // this compares that with the hash the approval recorded, so a schedule written to name
        // another diff than its proposal's (a ledger from elsewhere) applies nothing.
        match b.proposal_text(&p, "diff") {
            Ok(t) if sha(&t) == s.diff_sha256 => Ok(None),
            Ok(_) => Ok(Some(format!("proposal #{}'s diff is not the one approved", s.proposal))),
            Err(e) => Ok(Some(format!("{e:#}"))),
        }
    }

    /// Why the one who approved schedule `s` (of proposal `p`) may not have it applied now, if they may
    /// not: no account by that name, not a person's, or — unless it is wholly their own folder — no
    /// longer an administrator. Read as the socket reads a caller (socket::caller).
    fn approver_gone(&self, s: &Scheduled, p: &Entry) -> Result<Option<String>> {
        let by = s.approved_by.as_deref().unwrap_or_default();
        let read = |f: &std::path::Path| std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()));
        let who = match crate::socket::account(by, &read(&self.passwd)?, &read(&self.group)?) {
            Ok(w) => w,
            Err(e) => return Ok(Some(format!("its approver is not who they were: {e:#}"))),
        };
        if who.agent || !who.person {
            return Ok(Some(format!("its approver {by} is not a person's account")));
        }
        let tier: crate::policy::Tier = serde_json::from_value(json!(p.body.tier)).context("the proposal's tier")?;
        let files = p.body.data["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect();
        if !who.admin && !crate::socket::theirs(by, &crate::socket::Change { tier, files }) {
            return Ok(Some(format!("its approver {by} is no longer an administrator ({})", crate::socket::ADMINS)));
        }
        Ok(None)
    }

    fn refuse(&self, s: &Scheduled, why: &str) -> Result<Entry> {
        let why: String = why.chars().filter(|c| !c.is_control()).take(300).collect();
        self.b.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: s.approved_by.clone(),
            kind: "apply_refused".into(),
            tier: s.tier.clone(),
            tainted: false,
            reason: format!("Scheduled change not applied: {why}"),
            data: json!({ "proposal": s.proposal, "scheduled": s.seq }),
        })
    }

    /// Cancel schedule `seq` (its `apply_scheduled` entry): ledgered, then its timer stopped.
    pub fn cancel(&self, seq: u64, by: &str) -> Result<Entry> {
        if !policy::user_name_ok(by) {
            bail!("{by:?} is not a person's name");
        }
        let b = self.b;
        let lock = Lock::acquire(&b.state_dir)?;
        let Some(s) = self.scheduled()?.into_iter().find(|s| s.seq == seq) else {
            bail!("#{seq} is not a commit scheduled and waiting");
        };
        let e = b.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "apply_cancelled".into(),
            tier: s.tier.clone(),
            tainted: false,
            reason: format!("Cancelled the commit scheduled for {}: {}", s.at, s.reason),
            data: json!({ "proposal": s.proposal, "scheduled": s.seq, "timer": s.timer }),
        })?;
        // The ledger already says it is cancelled, and a run reads the ledger: a timer that cannot be
        // stopped (gone already, say) applies nothing.
        if !self.cancel.is_empty() {
            if let Err(err) = b.run_command(&lock, "commit-at timer stop", &self.cancel, &[("id", &s.timer.to_string())]) {
                eprintln!("note: {err:#} (cancelled all the same: its run finds the cancel in the ledger)");
            }
        }
        Ok(e)
    }

    /// At boot: the timers were transient, and are gone. A schedule from an earlier boot whose time
    /// is still ahead is armed again; one whose time passed while the machine was down is refused
    /// (never applied late). Each on its own: one that cannot be read or armed again is refused on
    /// the record, and the others go on. What it ledgered, if anything.
    pub fn resume(&self) -> Result<Vec<Entry>> {
        let b = self.b;
        let lock = Lock::acquire(&b.state_dir)?;
        let boot = b.boot_id();
        let mut out = Vec::new();
        for s in self.scheduled()?.into_iter().filter(|s| s.boot_id.is_none() || s.boot_id != boot) {
            let why = match time_of(&s) {
                Err(e) => format!("{e:#}"),
                Ok(at) if at <= chrono::Utc::now() => "the machine was not running at its time".into(),
                Ok(at) => match self.arm(&lock, s.timer, at) {
                    Ok(()) => continue,
                    Err(e) => format!("its timer could not be armed again after a restart: {e:#}"),
                },
            };
            out.push(self.refuse(&s, &why)?);
        }
        Ok(out)
    }
}
