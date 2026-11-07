//! torq-broker: the only writer of the TorqOS config repo.
//!
//! Lifecycle: propose (validated + ledgered, diff stored in the ledger) → apply (approved,
//! committed, rebuilt, ledgered) → revert (one change undone by a new commit, rebuilt, ledgered).
//! If a rebuild fails, the repo is reset to the previous commit, that commit is rebuilt (the
//! failed rebuild may already have activated), and the failure is ledgered.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

pub mod agent;
pub mod config;
pub mod remote;
pub mod snapshot;
pub mod ledger;
pub mod policy;
pub mod repo;
pub mod socket;

use anyhow::{bail, Context, Result};
use ledger::{Entry, Ledger, NewEntry};
use policy::{Actor, Tier};
use repo::Repo;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The largest user-tier data file the broker accepts (`Broker::check_user_data`).
const USER_DATA_MAX: u64 = 64 * 1024;
use std::os::fd::AsFd;
use std::process::{Command, Stdio};

pub struct Broker {
    pub repo: Repo,
    pub ledger: Ledger,
    state_dir: PathBuf,
    /// Shell command run after each commit: `{flake}` (the config repo at the commit), `{repo}` (its path).
    rebuild_cmd: String,
    /// The link to the running system (`/run/current-system`), recorded after every rebuild.
    current_system: PathBuf,
    /// The system profile (`/nix/var/nix/profiles/system`): what the next boot starts.
    system_profile: PathBuf,
    /// The `commit confirmed` commands (see config.rs for each).
    build_cmd: String,
    activate_cmd: String,
    snapshot_cmd: String,
    snapshot_dir: PathBuf,
    boot_cmd: String,
    schedule_cmd: String,
    /// This broker's own executable, which the scheduled rollback runs: the store path of the
    /// broker that made the change, not whatever `torq-broker` the new system has.
    exe: PathBuf,
    /// The kernel's boot ID (`/proc/sys/kernel/random/boot_id`), new on every boot.
    boot_id_file: PathBuf,
    /// Restore an anchor system with that system's own commands and PATH
    /// (`<system>/etc/torq/broker.json`), not those the change being undone installed.
    anchor_settings: bool,
    /// Where each person's password hash lives, outside the config repo (modules/accounts.nix).
    passwords_dir: PathBuf,
    /// What sets a password in the running system at once: `user:hash` lines on its input.
    password_cmd: String,
    /// The people the configuration declares: the only accounts `set_password` changes.
    people: Vec<String>,
    /// What the agent may propose (ADR-041).
    agent_limits: agent::Limits,
    /// The config repo's remote, if the owner named one (ADR-036).
    remote: Option<String>,
}

/// The fields the broker reads back from an entry, each in the shape the broker writes: anything else
/// in a ledger from elsewhere is refused before it is trusted.
fn shapes_ok(data: &serde_json::Value) -> Result<()> {
    let commits = ["commit", "anchor_commit", "reverted_commit", "from", "base", "before"];
    let systems = ["system", "anchor_system"];
    let check = |key: &str, v: &serde_json::Value| -> Result<()> {
        let Some(s) = v.as_str() else { return Ok(()) };
        if commits.contains(&key) {
            repo::revision(s).ok().filter(|r| r.len() == 40).with_context(|| format!("its {key} is not a commit"))?;
        }
        if systems.contains(&key) && !(s.starts_with("/nix/store/") && !s.contains("..") && s.bytes().all(|b| b.is_ascii_graphic())) {
            bail!("its {key} is not a store path");
        }
        if key == "label" && !snapshot::label_ok(s) {
            bail!("its label is not a snapshot's");
        }
        Ok(())
    };
    for (k, v) in data.as_object().into_iter().flatten() {
        check(k, v)?;
        for (k2, v2) in v.as_object().into_iter().flatten() {
            check(k2, v2)?;
        }
    }
    Ok(())
}

/// `data`, with the snapshot taken for it, if there is one: a change that failed has one too, and it
/// must stay reachable, as it holds a slot.
fn with_snapshot(mut data: serde_json::Value, snapshot: &serde_json::Value) -> serde_json::Value {
    if !snapshot.is_null() {
        data["snapshot"] = snapshot.clone();
    }
    data
}

/// A file of the broker's own, gone when it is dropped.
struct DropFile(PathBuf);
impl Drop for DropFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// What the last check of the running system's store paths found (`Broker::verify_store`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCheck {
    pub at: String,
    pub paths: usize,
    pub modified: Vec<String>,
}

/// A snapshot of the data, and the change it was taken before (ADR-038).
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub change: u64,
    pub ts: String,
    pub reason: String,
    pub label: String,
}

/// A `commit confirmed` waiting for `confirm`: an `applied` entry with a `confirm` record and no
/// `confirmed` or `rolled_back` for it. Derived from the ledger alone, never from a separate file.
#[derive(Debug, Clone, Serialize)]
pub struct PendingConfirm {
    pub applied: u64,
    /// What its timer names (`schedule_rollback`); none in a commit confirmed from before it had one.
    #[serde(skip)]
    timer: Option<u64>,
    pub deadline: String,
    /// The config and the system before the commit: what an unconfirmed commit goes back to.
    pub anchor_commit: String,
    pub anchor_system: String,
    /// The system the commit activated.
    pub system: Option<String>,
    /// Who approved it: the one who may confirm it.
    pub approved_by: Option<String>,
    /// The kernel's boot ID when it was applied: a rollback "by reboot" needs a different one.
    pub boot_id: Option<String>,
    #[serde(skip)]
    tier: String,
    #[serde(skip)]
    reason: String,
}

fn pending_confirm(entries: &[Entry]) -> Option<PendingConfirm> {
    let a = entries.iter().rev().find(|e| e.body.kind == "applied" && e.body.data.get("confirm").is_some())?;
    let seq = a.body.seq;
    let settled = entries
        .iter()
        // After it: an entry older than the change cannot settle it (a ledger someone else wrote could
        // hold one for a number yet to come).
        .any(|e| e.body.seq > seq && matches!(e.body.kind.as_str(), "confirmed" | "rolled_back") && e.body.data["applied"] == json!(seq));
    if settled {
        return None;
    }
    let c = &a.body.data["confirm"];
    Some(PendingConfirm {
        applied: seq,
        timer: c["timer"].as_u64(),
        deadline: c["deadline"].as_str()?.into(),
        anchor_commit: c["anchor_commit"].as_str()?.into(),
        anchor_system: c["anchor_system"].as_str()?.into(),
        system: a.body.data["system"].as_str().map(String::from),
        boot_id: c["boot_id"].as_str().map(String::from),
        approved_by: a.body.approved_by.clone(),
        tier: a.body.tier.clone(),
        reason: a.body.reason.clone(),
    })
}

/// The longest `commit confirmed` wait, in minutes (a day).
pub const MAX_CONFIRM_MINUTES: u32 = 24 * 60;

/// The largest diff a proposal may be (it is ledgered whole): 4 MiB, far beyond any configuration.
pub const MAX_DIFF: u64 = 4 << 20;
/// The agent's, smaller: a change it proposes is a person's to read (the audit of M3, M4).
pub const AGENT_MAX_DIFF: u64 = 1 << 20;
/// The agent's limits on the ledger (the audit of ADR-041): its reasons, its proposals waiting at
/// once, its refusals ledgered in an hour.
pub const AGENT_REASON_MAX: usize = 200;
pub const AGENT_WAITING_MAX: usize = 20;
pub const AGENT_REFUSALS_AN_HOUR: usize = 30;

/// How often the rollback of an unconfirmed commit may fail to take before it is ledgered as not
/// restored (below the timer's 60 tries, so the timer's own runs reach it).
pub const MAX_ROLLBACK_ATTEMPTS: u32 = 40;

/// One shell word, for a placeholder's value.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A command template with each `{name}` replaced by its shell-quoted value.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut cmd = template.to_string();
    for (k, v) in vars {
        cmd = cmd.replace(&format!("{{{k}}}"), &shell_quote(v));
    }
    cmd
}

/// Another broker operation holds the lock: try again later. Its own type, so the CLI can say
/// "busy" (exit 75) without matching error text.
#[derive(Debug)]
pub struct Busy(pub String);

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "another broker operation is running ({})", self.0)
    }
}

impl std::error::Error for Busy {}

/// An apply or revert that was interrupted and not yet reconciled.
#[derive(Debug, Clone, Serialize)]
pub struct Interrupted {
    pub op: String,
    pub target: u64,
}

/// A repository a new machine could adopt (ADR-036), as `adopt_inspect` found it.
#[derive(Debug, serde::Serialize)]
pub struct Adoptable {
    pub url: String,
    pub host: String,
    pub user: String,
    /// The remote its configuration names, if any.
    pub remote: Option<String>,
    pub entries: usize,
    pub head: String,
    /// The ledger commit checked.
    pub tip: String,
    /// The remote's host key, as pinned at first contact (its SHA-256 fingerprint): for the person to
    /// compare, since anyone answering first would be pinned.
    pub host_key: Option<String>,
    /// Changes it made (applied, reverted, repaired), of its `entries`.
    pub changes: usize,
    /// Whether its configuration turns SSH on, who has keys, and which Nix files it attaches: what the
    /// person is giving this machine to.
    pub ssh: bool,
    pub keyed: Vec<String>,
    pub files: Vec<String>,
}

/// How the machine compares with its ledger. See `Broker::status`.
#[derive(Debug)]
pub struct Status {
    pub ledger: std::result::Result<usize, String>,
    /// Where the ledger and the repository disagree about a change: each must be rewritten to agree
    /// with the other, and a rewrite of one is seen here.
    pub disagreements: Vec<String>,
    /// A torn last line is waiting: the next entry or push mends it.
    pub torn: bool,
    /// Store paths of the running system whose contents are not what the store recorded for them: the
    /// last daily check's (`verify-store`), or none if there has been none.
    pub store_modified: Vec<String>,
    /// The config repo's HEAD; None if git cannot read it.
    pub head: Option<String>,
    /// Uncommitted changes in the config repo (only the broker writes it).
    pub dirty: bool,
    /// The commit the ledger says the config repo is at.
    pub ledgered_commit: Option<String>,
    pub running_system: Option<String>,
    /// What the next boot starts (the system profile), if there is one.
    pub next_boot: Option<String>,
    /// The system the ledger says is running: recorded by init and after every rebuild (None if
    /// the last record has none, e.g. written by an older broker).
    pub ledgered_system: Option<String>,
    /// The last rebuild failed and could not restore the previous system (its ledger entry).
    pub unrestored: Option<String>,
    /// An apply or revert that was interrupted and not yet reconciled.
    pub interrupted: Option<Interrupted>,
    /// A broker operation is running (who holds the lock): mid-switch, the running system has
    /// changed before the ledger records it, so nothing is compared.
    pub busy: Option<String>,
    /// A commit confirmed waiting for `confirm`. Its boot default is, rightly, still the old system.
    pub pending_confirm: Option<PendingConfirm>,
    /// Its deadline passed more than two minutes ago and it has not been rolled back.
    pub overdue: bool,
    /// The last exchange with the config repo's remote (ADR-036), if it has one: read, not asked —
    /// status goes nowhere on the network.
    pub remote: Option<remote::Pushed>,
    /// A disagreement with the remote, kept until a person clears it.
    pub remote_alarm: Option<remote::Alarm>,
    /// The remote set, but its URL refused (not SSH): nothing is pushed.
    pub remote_refused: Option<String>,
}

impl Status {
    /// Everything that is not as the ledger says, worded for a person. Empty means all is well —
    /// or that it could not be checked because an operation is running (`busy`).
    pub fn problems(&self) -> Vec<String> {
        let mut p = self.drift();
        {
            p.extend(self.disagreements.iter().map(|d| format!("the ledger and the repository disagree: {d}")));
            if self.torn {
                p.push("the ledger's last line was cut short (a power cut?): the next change or push removes it, and says so".into());
            }
        }
        p
    }

    /// The machine against its ledger, and its remote. Not what the record says of
    /// itself, which `repair` has no business rebuilding for.
    pub fn drift(&self) -> Vec<String> {
        let mut p = Vec::new();
        if let Err(e) = &self.ledger {
            p.push(format!("ledger: {e}"));
        }
        // The remote (ADR-036). What a person reads here is this machine's own record: root can
        // rewrite it as it can the ledger. The check that root cannot reach is the remote itself —
        // its ledger branch, read from anywhere.
        if let Some(a) = &self.remote_alarm {
            p.push(format!(
                "this machine's ledger and its remote's disagree (since {}): {} — nothing more is pushed until a person looks into it and clears it (torq-broker remote-clear)",
                a.at, a.why
            ));
        }
        if let Some(why) = &self.remote_refused {
            p.push(format!("the remote is not used: {why}"));
        }
        if let Some(last) = &self.remote {
            let day_ago = chrono::Utc::now() - chrono::Duration::hours(24);
            let since = last.failing_since.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok());
            if !last.ok && since.is_some_and(|t| t < day_ago) {
                p.push(format!(
                    "nothing has reached the remote for a day or more (failing since {}; {}): {}",
                    last.failing_since.as_deref().unwrap_or("?"),
                    last.last_ok.as_deref().map_or("never pushed".to_string(), |t| format!("last pushed {t}")),
                    last.error.as_deref().unwrap_or("?")
                ));
            }
            if let Some(why) = last.main.as_deref().filter(|w| w.starts_with("the remote's main")) {
                p.push(format!("{why} — the ledger is pushed; the configuration there is behind"));
            }
        }
        if !self.store_modified.is_empty() {
            p.push(format!("the system's files in the store were changed after they were built ({}): `repair` puts them back", self.store_modified.iter().take(3).cloned().collect::<Vec<_>>().join(", ")));
        }
        // The rest compares the machine with its record, which an operation is changing: meaningless while
        // one runs. What the record says of itself, above, holds all the same.
        if self.busy.is_some() {
            return p;
        }
        if let Some(i) = &self.interrupted {
            p.push(format!("the {} of #{} was interrupted: run `torq-broker reconcile --approved-by <name>`", i.op, i.target));
        }
        if let Some(e) = &self.unrestored {
            p.push(format!(
                "the last rebuild ({e}) failed and could not restore the previous system: the running system \
                 may not match the config; fix the cause, then `torq-broker repair --approved-by <name>`"
            ));
        }
        match &self.head {
            None if self.ledgered_commit.is_some() => {
                p.push("config repo cannot be read (git rev-parse HEAD failed)".to_string())
            }
            Some(head) if Some(head) != self.ledgered_commit.as_ref() => p.push(format!(
                "config repo is at {head}, not the last ledgered commit {}: it was changed outside the broker",
                self.ledgered_commit.as_deref().unwrap_or("(none)")
            )),
            _ => {}
        }
        if self.dirty {
            p.push("config repo has uncommitted changes, made outside the broker".to_string());
        }
        if let Some(pc) = self.pending_confirm.as_ref().filter(|_| self.overdue) {
            p.push(format!(
                "change #{} (commit confirmed) was not confirmed by {} and has not been rolled back: \
                 `torq-broker expire-confirm` rolls it back",
                pc.applied, pc.deadline
            ));
        }
        if let Some(want) = &self.ledgered_system {
            if self.running_system.as_ref() != Some(want) {
                p.push(format!(
                    "running system is {}, not the last ledgered build {want}: it was switched outside the \
                     broker (e.g. another configuration built and switched to by hand, or an old boot entry)",
                    self.running_system.as_deref().unwrap_or("(unknown)")
                ));
            }
            let waiting_anchor = self.pending_confirm.as_ref().map(|pc| &pc.anchor_system);
            if let Some(next) = self.next_boot.as_ref().filter(|n| *n != want && Some(*n) != waiting_anchor) {
                p.push(format!(
                    "the next boot would start {next}, not the last ledgered build {want}: the boot default \
                     was changed outside the broker"
                ));
            }
        }
        p
    }
}

/// Exclusive lock so only one broker operation runs at a time. An OS file lock (flock), so the
/// kernel releases it when the last process holding it dies: a dead broker cannot wedge later ones.
struct Lock(fs::File);
impl Lock {
    /// Wait briefly for the lock: a status check holds it for milliseconds, and an operation that
    /// collides with one must not fail for it. A real operation holds it for minutes; after a few
    /// seconds, refuse and say who has it.
    fn acquire(dir: &Path) -> Result<Self> {
        Self::acquire_named(dir, "broker.lock", 3)
    }

    /// `name`, waited for up to `secs`.
    fn acquire_named(dir: &Path, name: &str, secs: u64) -> Result<Self> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        loop {
            if let Some(l) = Self::try_named(dir, name)? {
                return Ok(l);
            }
            if std::time::Instant::now() >= deadline {
                return Err(Busy(if name == "broker.lock" { Self::busy(dir) } else { format!("{} is held", dir.join(name).display()) }).into());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The lock, or None while another operation holds it.
    fn try_acquire(dir: &Path) -> Result<Option<Self>> {
        Self::try_named(dir, "broker.lock")
    }

    fn try_named(dir: &Path, name: &str) -> Result<Option<Self>> {
        fs::create_dir_all(dir)?;
        let p = dir.join(name);
        OpenOptions::new().write(true).create(true).truncate(false).open(&p)?;
        // Read-only: the fd is handed to the rebuild as stdin, and nothing should write into it.
        let f = fs::File::open(&p)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(Lock(f))),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e).context("locking the broker"),
        }
    }

    /// Who holds the lock, for messages.
    fn busy(dir: &Path) -> String {
        let p = dir.join("broker.lock");
        let h = holders(&p);
        format!("{}; held by: {}", p.display(), if h.is_empty() { "unknown".to_string() } else { h.join(", ") })
    }

    /// The lock as a child's stdin, so the rebuild's whole process tree holds it too. Killing the
    /// broker does not stop the build or the switch (`torq-switch`, its own unit), which may still
    /// switch the system; the next operation
    /// must wait for it rather than race it. A dup shares the open file description, which is what
    /// flock locks belong to, so the lock lasts until the last holder exits. The rebuild
    /// therefore has no terminal: anything that prompts (nix flake config, sudo, ssh host keys)
    /// reads EOF and fails, which is what a broker wants.
    fn for_child(&self) -> Result<Stdio> {
        Ok(Stdio::from(self.0.try_clone()?))
    }
}

/// Processes with `path` open. For the "operation is running" error (a process that inherited the
/// lock from a rebuild and outlived it would otherwise wedge the broker silently), and to tell a
/// stale git `index.lock` from a live one.
fn holders(path: &Path) -> Vec<String> {
    let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let me = std::process::id().to_string();
    let mut found = Vec::new();
    for proc in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let name = proc.file_name();
        // Skip ourselves (we have the broker lock open too, having just failed to lock it) and the
        // non-pid entries `self`/`thread-self`, which are links back to us.
        if name == me.as_str() || !name.to_string_lossy().bytes().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = fs::read_dir(proc.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t == path)) {
            let comm = fs::read_to_string(proc.path().join("comm")).unwrap_or_default();
            found.push(format!("{} ({})", proc.file_name().to_string_lossy(), comm.trim()));
        }
    }
    found
}

/// An apply or revert in flight, written to `state/pending.json` before its commit and removed
/// once its outcome is in the ledger. The commit happens before the (minutes-long) rebuild and the
/// ledger entry after it, so a broker that dies in between leaves a commit nobody ledgered; the
/// next operation finds this record and reconciles (see `Broker::reconcile_locked`).
#[derive(Debug, Serialize, Deserialize)]
struct Pending {
    /// The record's format (`PENDING_VERSION`); absent in records written before it existed (0).
    /// A broker refuses a record newer than it understands rather than misreading it.
    #[serde(default)]
    version: u32,
    /// "apply", "revert", or "expire" (the rollback of an unconfirmed commit).
    op: String,
    /// The proposal (apply) or applied entry (revert, expire) being acted on.
    target: u64,
    /// HEAD before the operation: the last ledgered config.
    before: String,
    /// Ledger length before the operation; an outcome entry would come after it.
    ledger_len: usize,
    /// The operation's commit, recorded as soon as it exists so it is never lost from the audit.
    commit: Option<String>,
    approved_by: String,
    tier: String,
    tainted: bool,
    /// A commit confirmed (apply) or its rollback (expire): the system to go back to — directly,
    /// not by rebuilding the old config.
    #[serde(default)]
    anchor_system: Option<String>,
    /// expire: how many times the restore has not taken.
    #[serde(default)]
    attempts: u32,
}

impl Pending {
    /// The ledger entry kinds that settle this operation, and the data key naming its target.
    fn outcome(&self) -> Result<(&'static [&'static str], &'static str)> {
        match self.op.as_str() {
            "apply" => Ok((&["applied", "apply_failed"], "proposal")),
            "revert" => Ok((&["reverted", "revert_failed"], "applied")),
            "expire" => Ok((&["rolled_back"], "applied")),
            other => bail!("pending.json names an unknown operation '{other}'"),
        }
    }
}

const PENDING_VERSION: u32 = 1;

/// First-boot setup in flight (`Broker::set_up`), written to `state/setup.json` before the config
/// repo gets its history and removed once change #1 is in the ledger. Setup switches before it
/// ledgers: a broker that dies after the switch leaves a machine that boots its new system with no
/// ledger. That system's `torq-broker-init` (or any broker operation) finds this record and
/// finishes setup (`Broker::finish_setup`). Its own file, not pending.json: an apply's record must
/// never start `torq-broker-init` (modules/broker.nix).
#[derive(Debug, Serialize, Deserialize)]
struct SetupRecord {
    /// Setup's commit, recorded as soon as it exists.
    commit: Option<String>,
}

/// fsync a directory, making a create, rename or unlink in it durable.
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// The commit the ledger says the config repo is at: that of the last entry that moved it.
/// (apply_failed / revert_failed carry none: the config is back at the previous commit.)
fn ledgered_commit(entries: &[Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.body.kind.as_str(), "init" | "adopted" | "applied" | "reverted" | "interrupted" | "repaired" | "rolled_back"))
        .find_map(|e| e.body.data.get("commit").and_then(|v| v.as_str()).map(String::from))
}

/// The broker runs each switch in a transient unit of its own (`systemd-run --pipe`, config.rs
/// REBUILD), which inherits the broker's lock through stdin. A switch running in it, or in the one
/// `nixos-rebuild` used (a system from before ADR-032, or one started by hand), is a switch the
/// broker must not race. The first is the one messages name.
const SWITCH_UNITS: [&str; 2] = ["torq-switch.service", "nixos-rebuild-switch-to-configuration.service"];
const SWITCH_UNIT: &str = SWITCH_UNITS[0];

fn switch_running() -> bool {
    SWITCH_UNITS.iter().any(|u| config::command("systemctl").args(["is-active", "--quiet", u]).status().is_ok_and(|s| s.success()))
}

impl Broker {
    pub fn new(repo: impl Into<PathBuf>, state_dir: impl Into<PathBuf>, rebuild_cmd: impl Into<String>) -> Self {
        let state_dir = state_dir.into();
        Self {
            repo: Repo::new(repo),
            ledger: Ledger::open(state_dir.join("ledger.jsonl")),
            state_dir,
            rebuild_cmd: rebuild_cmd.into(),
            current_system: PathBuf::from("/run/current-system"),
            system_profile: PathBuf::from("/nix/var/nix/profiles/system"),
            build_cmd: config::BUILD.into(),
            activate_cmd: config::ACTIVATE.into(),
            snapshot_cmd: String::new(),
            snapshot_dir: "/.snapshots".into(),
            boot_cmd: config::BOOT.into(),
            schedule_cmd: config::SCHEDULE.into(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("torq-broker")),
            boot_id_file: PathBuf::from("/proc/sys/kernel/random/boot_id"),
            anchor_settings: false,
            passwords_dir: PathBuf::from("/var/lib/torq/secrets/passwords"),
            password_cmd: "chpasswd -e".into(),
            people: Vec::new(),
            agent_limits: agent::Limits::default(),
            remote: None,
        }
    }

    /// The people the configuration declares (its normal users, from the broker's fixed settings).
    /// The config repo's remote (ADR-036): `torq.remote.url`, from broker.json.
    pub fn with_remote(mut self, url: Option<&str>) -> Self {
        self.remote = url.map(String::from);
        self
    }

    /// The remote, if there is one (its URL checked: SSH only).
    pub fn remote(&self) -> Result<Option<remote::Remote>> {
        self.remote.as_deref().map(|u| remote::Remote::new(u, &self.state_dir)).transpose()
    }

    /// After a change: the configuration and the ledger to the remote, if there is one (ADR-036).
    ///
    /// First the remote's ledger: its history must be one line (no merge), hold the commit checked
    /// last time, and every push since must start with the one before; then this ledger must start
    /// with the remote's newest. Anything else raises the alarm — sticky: nothing more is pushed
    /// until a person clears it (`remote-clear`, ledgered) — and so does a remote whose ledger has
    /// gone after one was pushed, or whose host key changed. Then the ledger, as a commit on top of
    /// the remote's newest, and `main` with it (not while a commit confirmed waits: it may yet be
    /// rolled back, and `main` is never rewound) — atomic, never forced. Busy: nothing (the operation
    /// pushes when it is done). Out of reach: recorded; the change stands.
    pub fn sync_remote(&self) -> Result<Option<remote::Pushed>> {
        let Some(r) = self.remote()? else { return Ok(None) };
        // One push at a time, under a lock of its own: what waits on the network is no operation's, and
        // holding the broker's across it let any account's requests keep every check busy (the audit of
        // M3, M1). Three exchanges of 60 s each at most: another push still running then is a stuck one.
        let _push = match Lock::acquire_named(&self.state_dir, "push.lock", 200) {
            Ok(l) => l,
            Err(e) if e.downcast_ref::<Busy>().is_some() => return Ok(None),
            Err(e) => return Err(e),
        };
        let now = || chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let before = r.last();
        let last_ok = before.as_ref().and_then(|p| if p.ok { Some(p.at.clone()) } else { p.last_ok.clone() });
        let failing_since = before.as_ref().and_then(|p| if p.ok { None } else { p.failing_since.clone().or(Some(p.at.clone())) }).unwrap_or_else(now);
        let fail = |error: String, anchor: Option<remote::Anchor>| remote::Pushed {
            at: now(),
            ok: false,
            error: Some(error),
            main: None,
            entries: 0,
            anchor,
            last_ok: last_ok.clone(),
            failing_since: Some(failing_since.clone()),
        };
        // Before anything reads it: a remote that had seen a torn line would never match again.
        if let Err(e) = self.ledger.mend() {
            let p = fail(format!("the ledger's last line is torn and could not be mended: {e:#}"), None);
            r.record(&p)?;
            return Ok(Some(p));
        }
        if !r.key().exists() {
            let p = fail("no key yet: request system remote key".into(), None);
            r.record(&p)?;
            return Ok(Some(p));
        }
        let ssh = r.ssh_command();
        let anchor = self.check_remote(&r, &ssh);
        if let remote::Anchor::Differs { why } = &anchor {
            r.raise(why)?;
        }
        if let Some(a) = r.alarm() {
            let p = fail(format!("not pushed: {}", a.why), Some(anchor));
            r.record(&p)?;
            return Ok(Some(p));
        }
        if let remote::Anchor::Unreachable { error } = &anchor {
            let p = fail(error.clone(), Some(anchor.clone()));
            r.record(&p)?;
            return Ok(Some(p));
        }
        // The ledger first, alone: whatever becomes of `main`, the ledger reaches the remote.
        let entries = self.ledger.read_all()?;
        let recorded = {
            let _held = self.ledger.shared();
            let texts = self.proposal_texts(&entries);
            self.ledger.text().and_then(|text| self.repo.record_ledger(&text, &texts, entries.len(), r.verified().as_deref()))
        };
        let ledger = recorded.and_then(|c| self.repo.push_ledger(&r.url, &ssh).map(|()| c));
        let commit = match ledger {
            Ok(c) => c,
            Err(e) => {
                let p = fail(format!("{e:#}"), Some(anchor));
                r.record(&p)?;
                return Ok(Some(p));
            }
        };
        r.set_verified(&commit)?;
        // Then `main` — never while it may yet be rewound: a commit confirmed waiting (its rollback),
        // an operation unfinished (its reconcile), a setup not yet done; never forced.
        // The commit is chosen under the broker's lock, where no operation is half done, and pushed after
        // it is let go: a commit settled then is one no rollback rewinds.
        let settled = match Lock::acquire(&self.state_dir) {
            Err(e) if e.downcast_ref::<Busy>().is_some() => Err("not pushed while an operation runs (the next push takes it)".to_string()),
            Err(e) => return Err(e),
            Ok(_l) => {
                if pending_confirm(&self.ledger.read_all()?).is_some() {
                    Err("not pushed while a commit confirmed waits (it may yet be rolled back)".to_string())
                } else if self.read_pending()?.is_some() || self.setup_path().exists() {
                    Err("not pushed while an operation is unfinished".to_string())
                } else {
                    Ok(self.repo.head()?)
                }
            }
        };
        let main = match settled {
            Err(why) => Some(why),
            Ok(rev) => self.repo.push_main(&r.url, &ssh, &rev).err().map(|e| format!("the remote's main did not take this configuration: {e:#}")),
        };
        let p = remote::Pushed { at: now(), ok: true, error: None, main, entries: entries.len(), anchor: Some(anchor), last_ok: Some(now()), failing_since: None };
        r.record(&p)?;
        Ok(Some(p))
    }

    /// What the remote's ledger says of this one (the checks of `sync_remote`), fetched now. On
    /// agreement, its newest commit is the one verified.
    fn check_remote(&self, r: &remote::Remote, ssh: &str) -> remote::Anchor {
        let differs = |why: String| remote::Anchor::Differs { why };
        let tip = match self.repo.fetch_remote_ledger(&r.url, ssh) {
            Err(e) => {
                let error = format!("{e:#}");
                // A host key that changed is not "out of reach": it is someone else answering, or a
                // key the owner rotated — a person decides which (the review of ADR-036).
                if error.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") || error.contains("Host key verification failed") {
                    return differs("the remote's host key is not the one pinned (someone else answering, or a new key: a person decides)".into());
                }
                return remote::Anchor::Unreachable { error };
            }
            Ok(None) if r.verified().is_some() => return differs("the remote's ledger is gone, though it was given one".into()),
            Ok(None) => return remote::Anchor::Empty,
            Ok(Some(tip)) => tip,
        };
        let walk = || -> Result<remote::Anchor> {
            if !self.repo.ledger_history_linear(&tip)? {
                return Ok(differs("the remote's ledger history has a merge in it: a rewrite set beside what it rewrites".into()));
            }
            let since = r.verified();
            if let Some(v) = &since {
                if !self.repo.has_ancestor(v, &tip)? {
                    return Ok(differs("the remote's ledger no longer holds the history checked before: it was rewritten".into()));
                }
            }
            // One ledger at a time: each push since the last check must start with the one before.
            let mut prev = since.as_deref().map(|v| self.repo.ledger_at_most(v, remote::MAX_LEDGER)).transpose()?;
            for (i, c) in self.repo.ledger_commits(since.as_deref(), &tip)?.iter().enumerate() {
                let text = self.repo.ledger_at_most(c, remote::MAX_LEDGER)?;
                if let Some(p) = &prev {
                    if let remote::Anchor::Differs { why } = remote::anchor(Some(p), &text) {
                        return Ok(differs(format!("the remote's ledger was rewritten by a later push ({} after the last check): {why}", i + 1)));
                    }
                }
                prev = Some(text);
            }
            let local = {
                let _held = self.ledger.shared();
                self.ledger.text().unwrap_or_default()
            };
            let a = remote::anchor(prev.as_deref(), &local);
            if matches!(a, remote::Anchor::Agrees { .. }) {
                r.set_verified(&tip)?;
            }
            Ok(a)
        };
        // Fetched, the remote's ledger is here to read: what cannot be read of it (no ledger.jsonl in a
        // commit, a broken object) is a disagreement, not "out of reach" (the second review of ADR-036).
        walk().unwrap_or_else(|e| differs(format!("the remote's ledger cannot be read: {e:#}")))
    }

    /// The remote's `incoming` configuration, fetched into the config repo (`request system pull`
    /// then loads it into a candidate): its commit, or None. Nothing is applied.
    pub fn fetch_incoming(&self) -> Result<Option<String>> {
        let r = self.remote()?.context("no remote is set: nothing to pull from")?;
        if !r.key().exists() {
            bail!("no key for the remote yet: request system remote key");
        }
        // Not from a remote whose ledger disagrees with this one's: what it offers is in question.
        if let Some(a) = r.alarm() {
            bail!("not pulled: this machine's ledger and the remote's disagree ({}) — a person looks into it first", a.why);
        }
        // A lock of its own, not the broker's: a fetch (up to two minutes) must not hold everyone's
        // changes back (the review of pull). Git takes objects and a ref while the broker commits.
        let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(self.state_dir.join("fetch.lock"))?;
        lock.try_lock().map_err(|_| anyhow::anyhow!("another fetch from the remote is running: try again when it is done"))?;
        self.repo.fetch_incoming(&r.url, &r.ssh_command())
    }

    /// The machine's key for a remote it is about to adopt from (first-boot setup, ADR-036): its
    /// public half, for the person to add to the repository. A host key pinned by an adoption given
    /// up is forgotten: this one starts from nothing.
    pub fn adopt_key(&self, url: &str, host: &str) -> Result<String> {
        if !self.ledger.read_all()?.is_empty() || self.repo.is_initialised() {
            bail!("this machine already has a configuration: only a new one adopts");
        }
        let r = remote::Remote::new(url, &self.state_dir)?;
        r.forget_host()?;
        r.public_key(host)
    }

    /// Fetch the repository at `url` into a staging repository and check it, changing nothing of the
    /// machine: what it is, for a person to read before adopting it.
    pub fn adopt_inspect(&self, url: &str) -> Result<Adoptable> {
        if !self.ledger.read_all()?.is_empty() || self.repo.is_initialised() {
            bail!("this machine already has a configuration: only a new one adopts");
        }
        let r = remote::Remote::new(url, &self.state_dir)?;
        if !r.key().exists() {
            bail!("no key for the remote yet");
        }
        let stage = Repo::new(self.state_dir.join("adopt.git"));
        stage.stage_from(url, &r.ssh_command())?;
        let mut a = self.check_stage(&stage, url)?;
        a.host_key = r.host_fingerprint();
        Ok(a)
    }

    /// The checks of a staged repository, none of them to the network. Its ledger: one line, each
    /// push extending the one before, the chain whole, nothing still waiting (a repository's ledger
    /// is someone else's text: an unsettled commit confirmed would drive the boot's rollback, as
    /// root), and `main` the commit it ends at. Its tree: no path no one may change, and this
    /// machine's own files plain files or absent (a link there would be followed, as root).
    pub fn check_stage(&self, stage: &Repo, url: &str) -> Result<Adoptable> {
        const MAX_LEDGER: u64 = remote::MAX_LEDGER;
        const MAX_PUSHES: usize = 100_000;
        const MAX_SETTING: u64 = 1 << 20;
        let tip = stage.commit_of("refs/torq/remote-ledger")?;
        if !stage.ledger_history_linear(&tip)? {
            bail!("the repository's ledger history has a merge in it: refused");
        }
        let commits = stage.ledger_commits(None, &tip)?;
        if commits.len() > MAX_PUSHES {
            bail!("the repository's ledger has {} pushes: more than a machine makes", commits.len());
        }
        let mut prev: Option<String> = None;
        for c in &commits {
            let text = stage.ledger_at_most(c, MAX_LEDGER)?;
            if let remote::Anchor::Differs { why } = remote::anchor(prev.as_deref(), &text) {
                bail!("the repository's ledger was rewritten along the way: {why}");
            }
            prev = Some(text);
        }
        // Its proposals' texts, beside its ledger (N19): each its hash's, none larger than a diff may be.
        for (name, size) in stage.ledger_text_sizes(&tip)? {
            if size > MAX_DIFF * 4 || name.len() != 64 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("the repository's ledger keeps proposals/{name:?}, which is not a proposal's text: refused");
            }
        }
        let text = prev.context("the repository has no ledger")?;
        if !text.ends_with('\n') {
            bail!("the repository's ledger ends in the middle of a line: refused");
        }
        let file = self.state_dir.join("adopt-ledger.jsonl");
        fs::write(&file, &text)?;
        let staged = Ledger::open(&file);
        let entries = staged.verify().context("the repository's ledger does not verify")?;
        let list = staged.read_all()?;
        // Every field a broker reads back, in the shape a broker writes it: a commit a commit, a system
        // a store path, a label digits. Text here is someone else's, and reaches git and the switch as
        // root (the review of 0.2.1, H1).
        for e in &list {
            shapes_ok(&e.body.data).with_context(|| format!("the repository's ledger, entry #{}", e.body.seq))?;
        }
        if pending_confirm(&list).is_some() {
            bail!("the repository's ledger has a commit confirmed still waiting: refused");
        }
        let head = stage.commit_of("refs/heads/main")?;
        for (mode, path) in stage.tree_paths(&head)? {
            if policy::tier_for_path(&path) == Tier::Forbidden {
                bail!("the repository holds {path:?}, which an adopted configuration may not: refused");
            }
            // Regular files only: Nix reads through a link, so a link could hold a setting this check
            // never sees — and a configuration has no need of one.
            if mode != "100644" && mode != "100755" {
                bail!("the repository's {path} is not a plain file: refused");
            }
        }
        // Settings the console reports on, read strictly: what cannot be read is a refusal, never "off".
        let setting = |p: &str| -> Result<serde_json::Value> {
            match stage.size_at(&head, p) {
                None => Ok(serde_json::Value::Null),
                Some(n) if n > MAX_SETTING => bail!("the repository's {p} is larger than a setting: refused"),
                Some(_) => {
                    let t = stage.text_at_most(&head, p, MAX_SETTING).with_context(|| format!("reading the repository's {p}"))?;
                    serde_json::from_str(&t).with_context(|| format!("the repository's {p} is not JSON this machine can read: refused"))
                }
            }
        };
        let torq = setting("nixos/torq.json")?;
        let user = torq["user"].as_str().filter(|u| policy::user_name_ok(u)).context("the repository names no owner")?.to_string();
        // This machine's hardware file must be read: attached at the top, as a new machine's is.
        let attached = setting("config-files.json")?;
        let names = |v: &serde_json::Value| -> Vec<String> {
            match v {
                serde_json::Value::String(s) => vec![s.clone()],
                serde_json::Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                _ => vec![],
            }
        };
        if !names(&attached[""]).iter().any(|f| f == "hardware-configuration.nix") {
            bail!("the repository's configuration does not read a hardware file: refused");
        }
        let files: Vec<String> = attached.as_object().into_iter().flat_map(|o| o.values()).flat_map(names).filter(|f| f != "hardware-configuration.nix").collect();
        let users = setting("nixos/users.json")?;
        let keyed: Vec<String> = users["users"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, u)| ["keys", "keyFiles"].iter().any(|k| u["openssh"]["authorizedKeys"][k].as_array().is_some_and(|l| !l.is_empty())))
            .map(|(n, _)| n.clone())
            .collect();
        // Every way the tree turns SSH on: NixOS's option, its alias, and the old list.
        let services = setting("nixos/services.json")?;
        let old_list = setting("services.json")?;
        let ssh = services["openssh"]["enable"] == json!(true)
            || services["sshd"]["enable"] == json!(true)
            || old_list.as_array().is_some_and(|l| l.iter().any(|s| matches!(s.as_str(), Some("ssh" | "openssh" | "sshd"))));
        let host = setting("system/host.json")?["host-name"].as_str().unwrap_or("?").to_string();
        if ledgered_commit(&list).as_deref() != Some(head.as_str()) {
            bail!("the repository's main is not the commit its ledger ends at");
        }
        Ok(Adoptable {
            url: url.to_string(),
            user,
            host,
            remote: torq["remote"]["url"].as_str().map(String::from),
            entries,
            changes: list.iter().filter(|e| matches!(e.body.kind.as_str(), "applied" | "reverted" | "repaired" | "adopted")).count(),
            head,
            tip,
            host_key: None,
            ssh,
            keyed,
            files,
        })
    }

    /// Become the machine staged by `adopt_inspect` — exactly that one, `head` and `tip` as the person
    /// was shown them, for `user`; nothing is fetched again (ADR-036): its history and its ledger,
    /// then this machine's own hardware file and software (`own`: hardware-configuration.nix,
    /// flake.nix, flake.lock) as a new commit, built and switched to, then ledgered as `adopted`.
    /// The ledger goes in last: a failure leaves the machine new, for setup to start again.
    pub fn adopt(&self, url: &str, own: &Path, head: &str, tip: &str, user: &str) -> Result<Entry> {
        if !self.ledger.read_all()?.is_empty() || self.repo.is_initialised() {
            bail!("this machine already has a configuration: only a new one adopts");
        }
        let lock = Lock::acquire(&self.state_dir)?;
        // What a setup cut short left is not this adoption's: the repo is absent (checked above).
        let _ = self.clear_setup();
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then adopt");
        }
        let r = remote::Remote::new(url, &self.state_dir)?;
        let stage_path = self.state_dir.join("adopt.git");
        let a = self.check_stage(&Repo::new(&stage_path), url)?;
        if a.head != head || a.tip != tip || a.user != user {
            bail!("what is staged is not what was shown: nothing was adopted");
        }
        let undo = || {
            let _ = fs::remove_dir_all(self.repo.root());
            let _ = fs::remove_dir_all(&stage_path);
            let _ = fs::remove_file(self.state_dir.join("adopt-ledger.jsonl"));
        };
        let commit = (|| -> Result<String> {
            self.repo.clone_stage(&stage_path)?;
            let files = ["hardware-configuration.nix", "flake.nix", "flake.lock"];
            for f in files {
                // A new file, whatever was at that name: nothing there is followed.
                let to = self.repo.root().join(f);
                match fs::remove_file(&to) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
                let bytes = fs::read(own.join(f)).with_context(|| format!("reading this machine's {f}"))?;
                fs::OpenOptions::new().write(true).create_new(true).open(&to).and_then(|mut w| std::io::Write::write_all(&mut w, &bytes)).with_context(|| format!("putting this machine's {f} in"))?;
            }
            self.repo.commit_files(&files, "This machine adopts this configuration: its hardware file and software\n\nActor: user\nTier: system")
        })()
        .inspect_err(|_| undo())?;
        let built = self.build_system(&lock, &commit).inspect_err(|_| undo()).context("the adopted configuration did not build: nothing was adopted")?;
        let switched = self.rebuild(&lock, &commit);
        if let Err(e) = &switched {
            let took = self.running_system().as_deref() == Some(built.as_str()) || self.next_boot().as_deref() == Some(built.as_str());
            if !took {
                undo();
                bail!("the switch failed, and the machine still runs and starts the image: nothing was adopted ({e:#})");
            }
        }
        // Its proposals' texts, each checked against its name, the hash its entries hold.
        let stage = Repo::new(&stage_path);
        for (name, _) in stage.ledger_text_sizes(&a.tip)? {
            let text = stage.ledger_text(&a.tip, &name)?;
            if self.keep_proposal_text(&text)? != name {
                bail!("the repository's proposals/{name} is not what its name says: nothing was adopted");
            }
        }
        // The old machine's history is this one's: in, whole, and this adoption on top of it.
        let staged = fs::read(self.state_dir.join("adopt-ledger.jsonl")).context("reading the staged ledger")?;
        let e = self.ledger.install(&staged, NewEntry {
            actor: "user".into(),
            approved_by: None,
            kind: "adopted".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!(
                "This machine adopted {} (its hardware file and software its own){}",
                a.host.chars().filter(|c| !c.is_control()).take(63).collect::<String>(),
                if switched.is_ok() { "" } else { "; the switch reported a problem: show system alarms, and restart if it still runs the image" }
            ),
            data: self.with_system(json!({ "commit": commit, "url": url, "from": a.head })),
        }).context("installing the ledger")?;
        // Done: from here nothing may undo it. What is left is housekeeping.
        let _ = r.set_verified(&a.tip);
        let _ = fs::remove_dir_all(&stage_path);
        let _ = fs::remove_file(self.state_dir.join("adopt-ledger.jsonl"));
        Ok(e)
    }

    /// `torq break-glass` (ADR-035): a root shell is about to open. On the record first — who and why
    /// — then off the machine, if it has a remote: the entry, and why the remote has not got it, if it
    /// has not. It takes no operation's lock: a stuck switch is when a person needs this most.
    pub fn break_glass(&self, by: &str, reason: &str) -> Result<(Entry, Option<String>)> {
        if !policy::user_name_ok(by) {
            bail!("who is breaking the glass: {by:?} is not a person's name");
        }
        if reason.trim().is_empty() || reason.chars().any(char::is_control) || reason.chars().count() > 200 {
            bail!("the reason is one line of text, at most 200 characters, not empty");
        }
        // Entry #1 would be taken for a machine set up, and setup and adopt would refuse it for good.
        if self.ledger.read_all()?.is_empty() {
            bail!("this machine is not set up yet: it has no ledger to put this in");
        }
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "break_glass".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: reason.into(),
            data: json!({}),
        })?;
        // The entry stands from here: a failure to send it is said, never taken for no record.
        let unsent = match self.sync_remote() {
            Ok(Some(p)) if p.ok => None,
            Ok(Some(p)) => Some(p.error.unwrap_or_else(|| "the push failed".into())),
            Ok(None) if matches!(self.remote(), Ok(None)) => None,
            Ok(None) => Some("an operation is running: it goes with the next push".into()),
            Err(e) => Some(format!("{e:#}")),
        };
        Ok((e, unsent))
    }

    /// The snapshots this machine still has, newest first, each with the change it came before.
    pub fn snapshots(&self) -> Result<Vec<Snapshot>> {
        let mut out = vec![];
        for e in self.ledger.read_all()?.iter().rev() {
            let Some(label) = e.body.data["snapshot"]["label"].as_str().filter(|l| snapshot::label_ok(l)) else { continue };
            if self.snapshot_dir.join(label).is_dir() {
                out.push(Snapshot { change: e.body.seq, ts: e.body.ts.clone(), reason: e.body.reason.clone(), label: label.to_string() });
            }
        }
        Ok(out)
    }

    /// A snapshot now, before work a person thinks better of (ADR-038): ledgered, so it is listed and
    /// can be restored from as any other. It counts towards what is kept.
    pub fn snapshot_now(&self, approved_by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        if self.snapshot_cmd.is_empty() {
            bail!("this machine does not take snapshots: its disk is not laid out for them, or `torq snapshots enable` is off");
        }
        let snapshot = self.take_snapshot(&lock, &Tier::System.to_string());
        if let Some(why) = snapshot["error"].as_str() {
            bail!("no snapshot was taken: {why}");
        }
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "snapshot_taken".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: "A snapshot of the data, by hand".into(),
            data: json!({ "snapshot": snapshot }),
        })
    }

    /// The snapshot of change `seq` gone, for the room or for what is in it. On the record first, as a
    /// password is: this is the one thing here that removes a way back, and that is never silent.
    pub fn delete_snapshot(&self, seq: u64, approved_by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        if self.snapshot_cmd.is_empty() {
            bail!("this machine does not take snapshots: nothing to delete");
        }
        let label = self.ledger.get(seq)?.body.data["snapshot"]["label"].as_str().filter(|l| snapshot::label_ok(l)).map(String::from).with_context(|| format!("change #{seq} has no snapshot"))?;
        if !self.snapshot_dir.join(&label).is_dir() {
            bail!("the snapshot of #{seq} has already gone");
        }
        let entry = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "snapshot_deleted".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("The snapshot from before #{seq} deleted"),
            data: json!({ "change": seq, "label": label }),
        })?;
        if let Err(e) = self.run_command(&lock, "snapshot", &self.snapshot_cmd, &[("action", "delete"), ("label", &label)]) {
            let gone = !self.snapshot_dir.join(&label).exists();
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "snapshot_delete_failed".into(),
                tier: Tier::System.to_string(),
                tainted: false,
                reason: format!("The snapshot from before #{seq} was not deleted{}", if gone { "" } else { ": some or all of it is still there" }),
                data: json!({ "change": seq, "label": label, "entry": entry.body.seq, "error": format!("{e:#}").chars().filter(|c| !c.is_control()).take(300).collect::<String>() }),
            })?;
            return Err(e);
        }
        Ok(entry)
    }

    /// Put `path` back as it was before change `seq` (ADR-038). Written by the owner of the folder it
    /// lies in, never by root: a person's folder is theirs to fill with links, and a root write into
    /// it would go where they point. Read by root, into a pipe: the owner holds nothing of the
    /// snapshot, which a descriptor would let them walk up. It goes to a name beside the target, and what
    /// is live changes by a rename only once it is all there: a failure leaves it as it was, and what
    /// was there is set aside, not overwritten. The broker's lock is for the checks alone: an owner can
    /// stop their half of the copy, and that must stop no one else. Ledgered, whether it worked or not.
    pub fn restore_data(&self, seq: u64, path: &str, by: &str) -> Result<Entry> {
        use std::os::unix::fs::MetadataExt;
        let lock = Lock::acquire(&self.state_dir)?;
        // Not on top of a change still in doubt: a machine going back while its data does would leave a
        // person guessing. A check at the start, no more: nothing holds one off during the copy.
        if pending_confirm(&self.ledger.read_all()?).is_some() {
            bail!("a commit confirmed is waiting: confirm it or let it roll back first");
        }
        let entry = self.ledger.get(seq)?;
        let label = entry.body.data["snapshot"]["label"].as_str().filter(|l| snapshot::label_ok(l)).map(String::from).with_context(|| format!("change #{seq} has no snapshot"))?;
        let (tree, parts) = snapshot::data_path(path)?;
        let path = snapshot::written_out(tree, &parts);
        let from = self.snapshot_dir.join(&label).join(tree);
        let (top, last) = snapshot::no_links(&from, &parts).with_context(|| format!("{path} as it was before #{seq}"))?;
        // A snapshot does not change, so what is read here stays so. A pipe would hang whoever opens it.
        if !last.is_dir() && !last.is_file() {
            bail!("{path} was neither a file nor a folder: not restored");
        }
        let uid = top.uid();
        if uid == 0 {
            bail!("{path} lies in a folder of root's: restoring it needs a root shell (torq break-glass)");
        }
        let gid = snapshot::primary_group(&fs::read_to_string("/etc/passwd").context("reading the accounts")?, uid).with_context(|| format!("no account has uid {uid} now, whose {path} this was"))?;
        let whole = parts.len() == 1;
        let live_top = snapshot::written_out(tree, &parts[..1]);
        let live = match fs::symlink_metadata(&live_top) {
            Ok(m) => Some(m),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && whole => None,
            Err(e) => return Err(e).with_context(|| format!("{live_top} is not there: restore that first")),
        };
        if live.as_ref().is_some_and(|m| m.is_symlink() || m.uid() != uid) {
            bail!("{live_top} has changed hands since the snapshot: not restored");
        }
        let set_aside = format!("{path}.before-{label}");
        if whole && live.is_some() && fs::symlink_metadata(&set_aside).is_ok() {
            bail!("{set_aside} is already there: move it first");
        }
        // Its own, for each run: nothing else makes two restores of one path keep apart.
        let stage = format!("{path}.restoring-{label}-{}", std::process::id());
        let copy = snapshot::Copy { source: from.join(parts.iter().collect::<PathBuf>()), to: &path, set_aside: &set_aside, stage: &stage, folder: last.is_dir(), mode: last.mode(), uid, gid, whole };
        drop(lock);
        let done = copy.run();
        let error = done.as_ref().err().map(|e| format!("{e:#}").chars().filter(|c| !c.is_control()).take(300).collect::<String>());
        // What happened, not what was expected: the swap decides, on what is there by then.
        let kept = done.is_ok() && fs::symlink_metadata(&set_aside).is_ok();
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "data_restored".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("{path} put back as it was before #{seq}{}", if error.is_some() { ": it failed" } else { "" }),
            data: json!({ "change": seq, "path": path, "label": label, "set_aside": kept.then_some(&set_aside), "error": error }),
        })?;
        done.with_context(|| format!("{path} was not restored"))?;
        Ok(e)
    }

    /// A person clears the remote's alarm, having looked into it: ledgered, and the next push checks
    /// the remote's whole history again (an alarm that is still true is raised again).
    pub fn clear_remote_alarm(&self, approved_by: &str) -> Result<Entry> {
        let r = self.remote()?.context("no remote is set")?;
        let _l = Lock::acquire(&self.state_dir)?;
        let why = r.alarm().map(|a| a.why).context("the remote has no alarm to clear")?;
        r.clear()?;
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "remote_cleared".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("The remote's alarm cleared: {}", why.chars().filter(|c| !c.is_control()).take(300).collect::<String>()),
            data: json!({ "url": r.url }),
        })
    }

    /// What the agent may propose (ADR-041): `torq.agent.mayPropose`, from broker.json.
    pub fn with_agent_limits(mut self, limits: &agent::Limits) -> Self {
        self.agent_limits = limits.clone();
        self
    }

    pub fn with_people(mut self, people: &[String]) -> Self {
        self.people = people.to_vec();
        self
    }

    /// Where password hashes go, and the command that sets one at once (tests: a scratch folder and
    /// a command that changes nothing).
    pub fn with_passwords(mut self, dir: impl Into<PathBuf>, cmd: impl Into<String>) -> Self {
        self.passwords_dir = dir.into();
        self.password_cmd = cmd.into();
        self
    }

    pub fn with_anchor_settings(mut self, on: bool) -> Self {
        self.anchor_settings = on;
        self
    }

    /// Where to read the boot ID (tests: a file they change to simulate a restart).
    pub fn with_boot_id_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.boot_id_file = path.into();
        self
    }

    fn boot_id(&self) -> Option<String> {
        fs::read_to_string(&self.boot_id_file).ok().map(|s| s.trim().to_string())
    }

    /// The `commit confirmed` commands: build, activation, boot default, schedule.
    pub fn with_confirm_commands(mut self, build: &str, activate: &str, boot: &str, schedule: &str) -> Self {
        (self.build_cmd, self.activate_cmd, self.boot_cmd) = (build.into(), activate.into(), boot.into());
        self.schedule_cmd = schedule.into();
        self
    }

    /// The broker the scheduled rollback runs (tests: the binary under test).
    pub fn with_exe(mut self, exe: impl Into<PathBuf>) -> Self {
        self.exe = exe.into();
        self
    }

    pub fn with_current_system(mut self, link: impl Into<PathBuf>) -> Self {
        self.current_system = link.into();
        self
    }

    /// Where the snapshots are, and where the data they hold is (a test's; a machine's are fixed).
    pub fn with_snapshot_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.snapshot_dir = dir.into();
        self
    }

    /// The command that snapshots the data before a change, with `{label}` (ADR-038). Empty: none.
    pub fn with_snapshots(mut self, cmd: impl Into<String>) -> Self {
        self.snapshot_cmd = cmd.into();
        self
    }

    pub fn with_system_profile(mut self, link: impl Into<PathBuf>) -> Self {
        self.system_profile = link.into();
        self
    }

    fn running_system(&self) -> Option<String> {
        fs::read_link(&self.current_system).ok().map(|p| p.to_string_lossy().into_owned())
    }

    /// The system the next boot starts: the profile's chain of links (`system` → `system-N-link`
    /// → store path), followed without requiring the store path itself to exist.
    fn next_boot(&self) -> Option<String> {
        let mut p = self.system_profile.clone();
        for _ in 0..8 {
            match fs::read_link(&p) {
                Ok(t) if t.is_absolute() => p = t,
                Ok(t) => p = p.parent()?.join(t),
                Err(_) => break,
            }
        }
        (p != self.system_profile).then(|| p.to_string_lossy().into_owned())
    }

    /// Add the system the last rebuild switched to, so `status` can tell when the machine is later
    /// switched to something else outside the broker.
    /// Snapshot the data before a change, for its entry: its label, or why there is none. A failure
    /// stops nothing: a disk too full to snapshot is one a change may be needed to clear, and the
    /// snapshot is a way back, not a guarantee. It is said, in the entry. Null: this machine has none,
    /// or the change is not one to take a snapshot for: only a system change, an administrator's with
    /// their password. Taking one for a person's own settings would let anyone push the others out.
    /// Taken once the commit stands: a change that never got that far uses no slot.
    fn take_snapshot(&self, lock: &Lock, tier: &str) -> serde_json::Value {
        if self.snapshot_cmd.is_empty() || tier == Tier::User.to_string() || tier == Tier::Observe.to_string() {
            return serde_json::Value::Null;
        }
        let label = chrono::Utc::now().timestamp_millis().to_string();
        match self.run_command(lock, "snapshot", &self.snapshot_cmd, &[("action", "take"), ("label", &label)]) {
            Ok(()) => json!({ "label": label }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        }
    }

    fn with_system(&self, mut data: serde_json::Value) -> serde_json::Value {
        data["system"] = self.running_system().into();
        data
    }

    /// Compare the machine with its ledger. Read-only, but under the lock: during an operation the
    /// comparison would be meaningless, so it reports `busy` instead.
    pub fn status(&self) -> Result<Status> {
        // Waiting as an operation does: a stream of quick ones (refused proposals, say) must not make
        // every check "busy" and so silent (the audit of M3, M1). Busy means a real operation.
        let lock = match Lock::acquire(&self.state_dir) {
            Ok(l) => Some(l),
            Err(e) if e.downcast_ref::<Busy>().is_some() => None,
            Err(e) => return Err(e),
        };
        let busy = lock.is_none().then(|| Lock::busy(&self.state_dir));
        self.facts(busy)
    }

    /// Each change the ledger says was made, against the repository's commit for it: there, approved
    /// by the same person, of the same proposal. By the entry's own `commit`, not the `Ledger-Entry`
    /// trailer, which an entry landing during an apply can leave one short. In HEAD's history, not just
    /// somewhere in the repository: a rewrite of `main` leaves the old commits behind. A commit rolled
    /// back is out of history on purpose.
    fn commits_disagree(&self, entries: &[Entry]) -> Vec<String> {
        const SAID: usize = 5;
        let all = match self.disagreeing(entries) {
            Ok(all) => all,
            Err(e) if self.repo.head().is_ok() => return vec![format!("the machine's history cannot be read: {e:#}")],
            Err(_) => return Vec::new(),
        };
        let more = all.len().saturating_sub(SAID);
        let mut said: Vec<String> = all.into_iter().skip(more).map(|(_, d)| d).collect();
        if more > 0 {
            said.insert(0, format!("{more} more before these"));
        }
        said
    }

    /// Each entry that disagrees, and how, that no one has looked at (`acknowledge_disagreements`).
    fn disagreeing(&self, entries: &[Entry]) -> Result<Vec<(u64, String)>> {
        let commits = self.repo.commit_trailers()?;
        // Only of what came before them: a ledger from elsewhere cannot settle what is yet to happen.
        let gone: std::collections::HashSet<u64> = entries.iter().filter(|e| e.body.kind == "rolled_back").filter_map(|e| e.body.data["applied"].as_u64().filter(|a| *a < e.body.seq)).collect();
        // What was looked into, word for word: a different disagreement about the same entry is new.
        let seen: std::collections::HashSet<&str> = entries.iter().filter(|e| e.body.kind == "disagreement_acknowledged").flat_map(|e| e.body.data["said"].as_array().into_iter().flatten()).filter_map(|s| s.as_str()).collect();
        Ok(entries
            .iter()
            .filter(|e| matches!(e.body.kind.as_str(), "applied" | "reverted") && !gone.contains(&e.body.seq))
            .filter_map(|e| {
                let seq = e.body.seq;
                let commit = e.body.data["commit"].as_str()?;
                let Some((by, proposal)) = commits.get(commit) else {
                    return Some((seq, format!("#{seq} names commit {commit}, which is not in the machine's history")));
                };
                if e.body.approved_by.as_deref() != Some(by.as_str()) {
                    return Some((seq, format!("#{seq} says {} approved it, its commit says {by:?}", e.body.approved_by.as_deref().unwrap_or("no one"))));
                }
                let wanted = e.body.data["proposal"].as_u64().map(|p| p.to_string());
                (wanted.is_some() && wanted.as_deref() != Some(proposal.as_str())).then(|| (seq, format!("#{seq} applied proposal #{}, its commit says {proposal:?}", wanted.unwrap_or_default())))
            })
            .filter(|(_, said)| !seen.contains(said.as_str()))
            .collect())
    }

    /// A person has looked into what the ledger and the repository disagree on: said no more, and on
    /// the record. A new disagreement is said as ever. Like clearing the remote's alarm, it decides
    /// nothing about who was right.
    pub fn acknowledge_disagreements(&self, approved_by: &str) -> Result<Entry> {
        let _lock = Lock::acquire(&self.state_dir)?;
        let entries = self.ledger.read_all()?;
        let list = self.disagreeing(&entries)?;
        if list.is_empty() {
            bail!("the ledger and the repository agree: nothing to acknowledge");
        }
        let reason = format!("{} looked into {} disagreement(s) between the ledger and the repository", approved_by, list.len());
        let (seqs, said): (Vec<u64>, Vec<String>) = list.into_iter().unzip();
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "disagreement_acknowledged".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason,
            data: json!({ "entries": seqs, "said": said }),
        })
    }

    /// The running system's store paths, each hashed against what the store recorded when it was
    /// built: a path changed in place after it was built (the store remounted writable, by root) is
    /// not one any rebuild looks at again, as it is already there (the audit of M3, M5). Run daily, and
    /// by `repair`; what it found is kept for `status`. What root also rewrites, the store's own record
    /// of the hashes, this does not see.
    pub fn verify_store(&self) -> Result<StoreCheck> {
        let system = self.running_system().context("the running system cannot be read")?;
        let out = config::command("nix-store").args(["--query", "--requisites", &system]).stderr(Stdio::inherit()).output().context("listing the running system's store paths")?;
        if !out.status.success() {
            bail!("listing the running system's store paths failed");
        }
        let paths: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
        let mut modified = Vec::new();
        for p in &paths {
            let o = config::command("nix-store").args(["--verify-path", p]).stdout(Stdio::null()).stderr(Stdio::null()).status().context("verifying a store path")?;
            if !o.success() {
                modified.push(p.clone());
            }
        }
        let c = StoreCheck { at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), paths: paths.len(), modified };
        fs::write(self.state_dir.join("store-check.json"), serde_json::to_vec(&c)?)?;
        Ok(c)
    }

    fn store_check(&self) -> Option<StoreCheck> {
        serde_json::from_slice(&fs::read(self.state_dir.join("store-check.json")).ok()?).ok()
    }

    /// Put changed store paths back as they were built: from the cache, or built again.
    fn repair_store(&self, modified: &[String]) -> Result<()> {
        if modified.is_empty() {
            return Ok(());
        }
        let ok = config::command("nix-store").arg("--repair-path").args(modified).stdin(Stdio::null()).stdout(Stdio::null()).status().context("repairing store paths")?.success();
        if !ok {
            bail!("the store paths could not all be repaired: {}", modified.join(", "));
        }
        Ok(())
    }

    /// The comparison itself, for a caller that already holds the lock (or knows it is busy).
    fn facts(&self, busy: Option<String>) -> Result<Status> {
        let ledger = self.ledger.verify().map_err(|e| format!("{e:#}"));
        let entries = self.ledger.read_all().unwrap_or_default();
        let interrupted = match self.read_pending()? {
            Some(p) if !self.outcome_recorded(&p)? => Some(Interrupted { op: p.op, target: p.target }),
            _ => None,
        };
        // The latest entry that recorded a system (init, or any rebuild). A null there (the link
        // could not be read) means unknown — not "fall back to an older one".
        let last_record = entries.iter().rev().find(|e| e.body.data.get("system").is_some());
        let unrestored = last_record
            .filter(|e| e.body.data.get("restored") == Some(&json!(false)))
            .map(|e| format!("#{} {}", e.body.seq, e.body.kind));
        let pending_confirm = pending_confirm(&entries);
        let overdue = pending_confirm.as_ref().is_some_and(|pc| {
            chrono::DateTime::parse_from_rfc3339(&pc.deadline)
                .is_ok_and(|d| chrono::Utc::now() > d + chrono::Duration::minutes(2))
        });
        Ok(Status {
            pending_confirm,
            overdue,
            remote: self.remote().ok().flatten().and_then(|r| r.last()),
            remote_alarm: self.remote().ok().flatten().and_then(|r| r.alarm()),
            remote_refused: self.remote().err().map(|e| format!("{e:#}")),
            disagreements: self.commits_disagree(&entries),
            torn: self.ledger.torn(),
            store_modified: self.store_check().map(|c| c.modified).unwrap_or_default(),
            ledger,
            head: self.repo.head().ok(),
            dirty: self.repo.ensure_clean().is_err() && self.repo.head().is_ok(),
            ledgered_commit: ledgered_commit(&entries),
            running_system: self.running_system(),
            next_boot: self.next_boot(),
            ledgered_system: last_record.and_then(|e| e.body.data["system"].as_str().map(String::from)),
            unrestored,
            interrupted,
            busy,
        })
    }

    /// Return the machine to its last ledgered configuration (ADR-012): the person's explicit way
    /// back after drift — a system switched outside the broker, a changed boot default, a failed
    /// restore. Never run automatically. An interrupted operation is finished by reconciling it,
    /// which is the repair. A config repo changed outside the broker is refused: a rebuild would
    /// build that change, so a person must look at it first.
    pub fn repair(&self, by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot repair on a broken ledger")?;
        if let Some(e) = self.reconcile_locked(&lock, by)? {
            return Ok(e);
        }
        // A waiting commit confirmed is not drift; its own rollback is what undoes it.
        self.refuse_if_confirm_pending()?;
        let head = self.repo.head()?;
        let entries = self.ledger.read_all()?;
        let Some(ledgered) = ledgered_commit(&entries) else {
            bail!("this machine's ledger has no configuration yet: initialise it first (torq-broker init)");
        };
        if ledgered != head || self.repo.ensure_clean().is_err() {
            bail!(
                "the config repo was changed outside the broker (it is at {head}, the ledger says {ledgered}); \
                 a rebuild would build that change, so look at {} first",
                self.repo.root().display()
            );
        }
        let before = self.facts(None)?;
        if before.drift().is_empty() {
            bail!("nothing to repair: the machine is as its ledger says");
        }
        // A manual nixos-rebuild does not take the broker lock: switching now would race it, and the
        // result could be its system recorded as ours.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then repair");
        }
        // What a rebuild does not look at again: store paths it already has, changed in place.
        let store = self.repair_store(&before.store_modified);
        let rebuilt = self.rebuild(&lock, &head).is_ok() && store.is_ok();
        if store.is_ok() && !before.store_modified.is_empty() {
            let _ = self.verify_store();
        }
        // A switch sets the running system and the next boot alike; if they differ afterwards,
        // something else changed one of them, and this was not a repair.
        // A switch started meanwhile would set both alike, so look for one too; and a profile that
        // vanished during the repair is not one we can vouch for.
        let (running, next) = (self.running_system(), self.next_boot());
        let profile_ok = if before.next_boot.is_some() { next.is_some() && next == running } else { next.is_none() || next == running };
        let restored = rebuilt && profile_ok && !switch_running();
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "repaired".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: if restored {
                "Returned the machine to its ledgered configuration".into()
            } else {
                "Tried to return the machine to its ledgered configuration, but it did not take".into()
            },
            // What was wrong is ledgered too: repairing must not erase the evidence of drift.
            data: self.with_system(json!({
                "commit": head,
                "restored": restored,
                "drift": {
                    "running_system": before.running_system,
                    "next_boot": before.next_boot,
                    "ledgered_system": before.ledgered_system,
                    "problems": before.drift(),
                },
            })),
        })?;
        if !restored {
            bail!("the repair did not take: the machine may still not match its ledger (recorded as #{})", e.body.seq);
        }
        Ok(e)
    }

    /// First-boot setup (docs/INSTALL.md): take over the config repo setup has just written, build it,
    /// switch to it, and ledger it as change #1 with the system it really switched to (`init` records
    /// the running system as the configuration's, true only where the machine already runs it). Once
    /// there is a ledger, refused: setup is once.
    /// - Built first: a configuration that does not build changes nothing — not taken over (its
    ///   history removed, its files left), so setup can correct its answers and try again.
    /// - Then switched to. If the new system took — it runs, or it is the boot default (a switch sets
    ///   that before activating) — the machine starts it from now on, and its history must name it:
    ///   ledgered, a problem the switch reported said in the entry (`status` shows any difference,
    ///   `repair` returns the machine to it). If it took neither way (the profile not set, the boot
    ///   loader not installed), the machine is still the image, with no account: nothing is set up,
    ///   as for a build that failed, and setup asks again (audit A4).
    pub fn set_up(&self) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        // A setup cut short on the image starts again from nothing (the console removes what it
        // left, the repo included): its record goes too.
        if self.setup_record()?.is_some() && !self.repo.is_initialised() {
            self.clear_setup()?;
        }
        self.refuse_if_interrupted()?;
        if !self.ledger.read_all()?.is_empty() {
            bail!("this machine is already set up: it has a ledger");
        }
        // Only a repo setup has just written, never one with a history of its own: a failed build
        // removes the history this call made, and must never remove anything else.
        if self.repo.is_initialised() {
            bail!("the config repo already has a history: setup takes over only a repo first-boot setup has just written");
        }
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then set up");
        }
        // Recorded before the repo has a history, and its commit as soon as it exists: from the
        // switch on, the machine may start the new system, which finishes setup if this broker dies.
        self.write_setup(&SetupRecord { commit: None })?;
        self.repo.init()?;
        let head = self.repo.head()?;
        self.write_setup(&SetupRecord { commit: Some(head.clone()) })?;
        let built = match self.build_system(&lock, &head) {
            Ok(system) => system,
            Err(e) => {
                let _ = std::fs::remove_dir_all(self.repo.root().join(".git"));
                let _ = self.clear_setup();
                return Err(e.context("the new configuration did not build: nothing was set up"));
            }
        };
        let switched = self.rebuild(&lock, &head);
        if let Err(e) = &switched {
            let took = self.running_system().as_deref() == Some(built.as_str()) || self.next_boot().as_deref() == Some(built.as_str());
            if !took {
                let _ = std::fs::remove_dir_all(self.repo.root().join(".git"));
                let _ = self.clear_setup();
                bail!("the switch to the new system failed, and the machine still runs and starts the image: nothing was set up ({e:#})");
            }
        }
        let reason = if switched.is_ok() { "TorqOS set up" } else { "TorqOS set up (the switch reported a problem: show system alarms)" };
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: None,
            kind: "init".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: reason.into(),
            data: self.with_system(json!({ "commit": head })),
        })?;
        self.clear_setup()?;
        Ok(e)
    }

    /// `request system password` (SECURITY-MODEL.md, "Accounts and passwords"): a person's new
    /// password, as a hash, set in the running system at once, then kept in the file their account
    /// points at — outside the config repo, readable by the system only — so every later build keeps
    /// it. Ledgered: whose password, changed by whom; never the hash.
    pub fn set_password(&self, user: &str, hash: &str, by: &str) -> Result<Entry> {
        let _lock = Lock::acquire(&self.state_dir)?;
        self.ledger.verify().context("cannot change a password on a broken ledger")?;
        // A person the configuration declares only: never root or a system account, whose password
        // nothing keeps (accounts.nix points people's accounts at their files only).
        if !policy::user_name_ok(user) || !self.people.iter().any(|p| p == user) {
            bail!("'{user}' is not a person's account on this machine");
        }
        if !policy::password_hash_ok(hash) {
            bail!("not a password hash this machine reads (SHA-512 or yescrypt crypt)");
        }
        // On the record first: stopped between here and the change, the ledger says a little too much,
        // and never too little. A password that changed with nothing to say so is the worse way round.
        let entry = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "password".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("Password changed for {user}"),
            data: json!({ "user": user }),
        })?;
        let Err((running, e)) = self.store_password(user, hash) else { return Ok(entry) };
        let reason = if running {
            format!("The password for {user} is set in the running system but may not be kept: the next start or build may bring the old one back")
        } else {
            format!("The password was not changed for {user}, after all")
        };
        let said = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: Some(by.into()),
            kind: "password_failed".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason,
            data: json!({ "user": user, "entry": entry.body.seq, "running": running, "error": format!("{e:#}").chars().filter(|c| !c.is_control()).take(300).collect::<String>() }),
        });
        Err(match said {
            Ok(_) => e,
            Err(l) => e.context(format!("and the ledger could not say so: {l:#}")),
        })
    }

    /// A new hash, written whole beside where it is kept, then set in the running system, then put in
    /// place, so what can fail mostly fails first. The error says whether the running system has it.
    fn store_password(&self, user: &str, hash: &str) -> std::result::Result<(), (bool, anyhow::Error)> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let part = self.passwords_dir.join(format!(".{user}.new"));
        let staged = (|| -> Result<()> {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.passwords_dir)?;
            let _ = std::fs::remove_file(&part);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            f.write_all(format!("{hash}\n").as_bytes())?;
            f.sync_all()?;
            Ok(())
        })();
        staged.map_err(|e| (false, e))?;
        // At once: the running system's own tool. It fails for an account this machine does not have.
        let set = (|| -> Result<()> {
            let mut words = self.password_cmd.split_whitespace();
            let mut cmd = config::command(words.next().unwrap_or("chpasswd"));
            cmd.args(words).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
            let mut child = cmd.spawn().context("setting the password")?;
            child.stdin.take().context("setting the password")?.write_all(format!("{user}:{hash}\n").as_bytes())?;
            let out = child.wait_with_output()?;
            if !out.status.success() {
                bail!("the password was not set: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            Ok(())
        })();
        if let Err(e) = set {
            let _ = std::fs::remove_file(&part);
            return Err((false, e));
        }
        // A rename is only as lasting as its directory.
        let kept = std::fs::rename(&part, self.passwords_dir.join(user)).map_err(anyhow::Error::from).and_then(|()| Ok(fs::File::open(&self.passwords_dir)?.sync_all()?));
        kept.context("keeping the password").map_err(|e| (true, e))
    }

    pub fn init(&self) -> Result<()> {
        // Before setup goes on, or starts again: a power cut may be why it is here.
        self.ledger.mend().context("mending the ledger")?;
        // A first-boot setup cut short after its switch: finished here, at the new system's start
        // (torq-broker-init). While setup itself holds the lock — its own switch starts this unit —
        // there is nothing to do: setup finishes, or its record stays for the next start.
        if self.setup_record()?.is_some() {
            let Some(lock) = Lock::try_acquire(&self.state_dir)? else { return Ok(()) };
            return self.finish_setup(&lock).map(drop);
        }
        let _l = Lock::acquire(&self.state_dir)?;
        self.refuse_if_interrupted()?;
        // Once there is a ledger, init changes nothing: its baseline commit (`git add -A`) would take
        // whatever is in the working tree into the history, unledgered (audit of 2026-10-31).
        if !self.ledger.read_all()?.is_empty() {
            return Ok(());
        }
        self.repo.init()?;
        {
            self.ledger.append(NewEntry {
                actor: "user".into(),
                approved_by: None,
                kind: "init".into(),
                tier: Tier::System.to_string(),
                tainted: false,
                reason: "Config repository initialised".into(),
                data: self.with_system(json!({ "commit": self.repo.head()? })), // the baseline for `status`
            })?;
        }
        Ok(())
    }

    /// User-tier files are data by construction, whoever proposes them: a regular file (git mode
    /// 100644 — never a symlink) holding exactly the JSON shape its leaf takes
    /// (`policy::user_leaf_ok`), at most `USER_DATA_MAX` bytes. Deleting one is always fine. The
    /// NixOS module reading the file is still the authority on what the data means; this makes sure
    /// it is the right kind of data, refused now rather than failing the rebuild.
    fn check_user_data(&self, diff_path: &Path, files: &[String]) -> Result<()> {
        let user: Vec<String> = files.iter().filter(|f| policy::is_user_data(f)).cloned().collect();
        if user.is_empty() {
            return Ok(());
        }
        for (path, file) in self.repo.proposed_files(diff_path, &user, USER_DATA_MAX)? {
            let Some(f) = file else { continue };
            let leaf = path.rsplit('/').next().unwrap_or_default();
            let data = f.mode == "100644"
                && f.size <= USER_DATA_MAX
                && serde_json::from_slice::<serde_json::Value>(&f.content).is_ok_and(|v| policy::user_leaf_ok(leaf, &v));
            if !data {
                bail!(
                    "refused: {path} must be a regular file holding {} of at most {} KiB",
                    match leaf {
                        "packages.json" => format!("a JSON list of package names (at most {})", policy::MAX_USER_PACKAGES),
                        _ => "JSON data".to_string(),
                    },
                    USER_DATA_MAX / 1024
                );
            }
        }
        Ok(())
    }

    /// Why the agent may not propose the change in `diff_path` (touching `files`), or None: each file
    /// as HEAD has it and as the change leaves it, read as JSON; a file that is not JSON is no
    /// setting, and never the agent's.
    fn agent_refusal(&self, diff_path: &Path, files: &[String]) -> Result<Option<String>> {
        const MAX: u64 = 1 << 20;
        let json = |bytes: &[u8]| serde_json::from_slice::<serde_json::Value>(bytes).ok();
        let mut touched = Vec::new();
        for (path, after) in self.repo.proposed_files(diff_path, files, MAX)? {
            let not_a_setting = || Ok(Some(format!("{path}: not a setting (a regular JSON file of the tree) — never the agent's")));
            let before = match self.repo.file_at_head(&path, MAX) {
                Ok(None) => None,
                Ok(Some(b)) => match json(&b) {
                    Some(v) => Some(v),
                    None => return not_a_setting(),
                },
                Err(_) => return not_a_setting(),
            };
            let after = match after {
                None => None,
                Some(f) if f.mode == "100644" && f.size <= MAX => match json(&f.content) {
                    Some(v) => Some(v),
                    None => return not_a_setting(),
                },
                Some(_) => return not_a_setting(),
            };
            touched.push(agent::Touched { path, before, after });
        }
        Ok(agent::refusal(&self.agent_limits, &touched))
    }

    pub fn propose(&self, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        self.propose_on(None, diff_path, reason, actor, tainted)
    }

    /// Propose a change prepared against the configuration at commit `base`. It is refused now if
    /// the configuration has moved on since, and again at `apply` if it moves before then: applied
    /// on top of a newer configuration, a change prepared against an older one can silently undo or
    /// mix with the commit in between. Without a base, the diff is applied wherever it applies.
    pub fn propose_on(&self, base: Option<&str>, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
        let proposed = self.propose_locked(base, diff_path, reason, actor, tainted);
        // Every refusal of the agent's is on the record, whatever refused it — the forbidden paths,
        // bad data, a diff that does not apply, the list (the audit of ADR-041): an agent that tries
        // is seen trying. (Busy is not a refusal: nothing was looked at.)
        if actor == Actor::Agent {
            if let Err(e) = &proposed {
                let _ = self.record_refusal(diff_path, reason, tainted, &format!("{e:#}"));
            }
        }
        proposed
    }

    /// The agent's refusals, ledgered (kind `refused`): why, the diff's hash, its reason shortened —
    /// never the diff. At most `AGENT_REFUSALS_AN_HOUR` an hour: past that they are turned away
    /// without an entry each, so that refusals cannot fill the ledger.
    fn record_refusal(&self, diff_path: &Path, reason: &str, tainted: bool, why: &str) -> Result<()> {
        let entries = self.ledger.read_all()?;
        // Only once the machine's ledger has begun (its `init`): before, a refusal would be entry #1,
        // and setup and init would take the ledger for one already set up (the second review).
        if entries.first().is_none_or(|e| e.body.kind != "init") {
            return Ok(());
        }
        let hour_ago = chrono::Utc::now() - chrono::Duration::hours(1);
        let recent = entries
            .iter()
            .filter(|e| e.body.kind == "refused" && e.body.actor == "agent")
            .filter(|e| chrono::DateTime::parse_from_rfc3339(&e.body.ts).is_ok_and(|t| t > hour_ago))
            .count();
        if recent >= AGENT_REFUSALS_AN_HOUR {
            return Ok(());
        }
        let mut diff = Vec::new();
        if let Ok(f) = fs::File::open(diff_path) {
            let _ = std::io::Read::read_to_end(&mut std::io::Read::take(f, MAX_DIFF), &mut diff);
        }
        let one_line = |s: &str, n: usize| s.chars().map(|c| if c.is_control() { '?' } else { c }).take(n).collect::<String>();
        let reason = one_line(reason, AGENT_REASON_MAX);
        self.ledger.append(NewEntry {
            actor: Actor::Agent.to_string(),
            approved_by: None,
            kind: "refused".into(),
            tier: Tier::System.to_string(),
            tainted,
            reason: if reason.trim().is_empty() { "(no reason)".into() } else { reason },
            data: json!({ "diff_sha256": hex::encode(Sha256::digest(&diff)), "why": one_line(why, 1000) }),
        })?;
        Ok(())
    }

    /// Where a proposal's diff and what it does are kept: beside the ledger, by their hash, so the
    /// ledger stays small however big they are (the audit of M3, M4; N19). The remote gets them with
    /// the ledger (`sync_remote`).
    fn proposals_dir(&self) -> PathBuf {
        self.state_dir.join("proposals")
    }

    /// `text`, kept by its hash; the hash.
    fn keep_proposal_text(&self, text: &str) -> Result<String> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let sha = hex::encode(Sha256::digest(text.as_bytes()));
        let dir = self.proposals_dir();
        fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let to = dir.join(&sha);
        if to.exists() {
            return Ok(sha);
        }
        let part = dir.join(format!(".{sha}.new"));
        let _ = fs::remove_file(&part);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&part, &to)?;
        fs::File::open(&dir)?.sync_all()?;
        Ok(sha)
    }

    /// A proposal's `diff` or `effect`: in its entry, as the ledger once kept them, or beside it by
    /// hash, checked against the hash the entry holds.
    pub fn proposal_text(&self, e: &Entry, what: &str) -> Result<String> {
        if let Some(t) = e.body.data[what].as_str() {
            return Ok(t.to_string());
        }
        let sha = e.body.data[format!("{what}_sha256")].as_str().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).with_context(|| format!("proposal #{} names no {what}", e.body.seq))?;
        let text = fs::read_to_string(self.proposals_dir().join(sha)).with_context(|| format!("proposal #{}'s {what} is not kept here", e.body.seq))?;
        if hex::encode(Sha256::digest(text.as_bytes())) != sha {
            bail!("proposal #{}'s {what} is not what its entry names: it was changed", e.body.seq);
        }
        Ok(text)
    }

    /// Every proposal's texts the ledger names and this machine keeps: what goes to the remote with it.
    pub fn proposal_texts(&self, entries: &[Entry]) -> Vec<(String, String)> {
        let mut out = std::collections::BTreeMap::new();
        for e in entries.iter().filter(|e| e.body.kind == "proposal") {
            for what in ["diff", "effect"] {
                if e.body.data[what].is_string() {
                    continue;
                }
                if let (Some(sha), Ok(t)) = (e.body.data[format!("{what}_sha256")].as_str(), self.proposal_text(e, what)) {
                    out.insert(sha.to_string(), t);
                }
            }
        }
        out.into_iter().collect()
    }

    fn propose_locked(&self, base: Option<&str>, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        self.refuse_if_interrupted()?;
        // An agent always says what it prepared its change against: without a base, a diff applies on
        // top of whatever the configuration has become (second audit, M2; KNOWN-GAPS 6g).
        if actor == Actor::Agent && base.is_none() {
            bail!("refused: an agent's proposal needs --base (the commit it was prepared against)");
        }
        // A reason is one line of text: it becomes the commit's subject, above the trailers the
        // ledger is checked against (Approved-By, Ledger-Entry): a line break or control character
        // could forge one (audit of 2026-11-01, N4).
        if reason.trim().is_empty() || reason.chars().any(char::is_control) {
            bail!("refused: the reason is one line of text (no line breaks or control characters), not empty");
        }
        if actor == Actor::Agent {
            // Short, and not many waiting at once: the agent cannot fill the ledger, or bury one
            // proposal under others (the audit of ADR-041).
            if reason.chars().count() > AGENT_REASON_MAX {
                bail!("refused: the agent's reason is at most {AGENT_REASON_MAX} characters");
            }
            let entries = self.ledger.read_all()?;
            // Only those still able to land: an agent's proposal carries its base, and one prepared
            // against an older configuration can never be applied (the second review).
            let head = self.repo.head().ok();
            let done: std::collections::HashSet<u64> = entries
                .iter()
                .filter(|e| matches!(e.body.kind.as_str(), "applied" | "apply_failed"))
                .filter_map(|e| e.body.data["proposal"].as_u64())
                .collect();
            let waiting = entries
                .iter()
                .filter(|p| p.body.kind == "proposal" && p.body.actor == "agent" && !done.contains(&p.body.seq))
                .filter(|p| head.is_some() && p.body.data["base"].as_str() == head.as_deref())
                .count();
            if waiting >= AGENT_WAITING_MAX {
                bail!("refused: {waiting} of the agent's proposals are waiting for a person already (at most {AGENT_WAITING_MAX})");
            }
        }
        self.refuse_unledgered_head()?;
        if let Some(base) = base {
            // A full commit id, never a ref (`HEAD`, a branch) that would always "match".
            if base.len() != 40 || !base.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
                bail!("--base must be a full commit id (40 lowercase hex digits), not {base:?}");
            }
            self.refuse_stale_base(base)?;
        }
        // A change is small: a diff far bigger is not one, and would be held in memory and ledgered
        // whole by a root process (second audit, M4).
        let size = fs::metadata(diff_path).with_context(|| format!("reading diff {}", diff_path.display()))?.len();
        if size > MAX_DIFF {
            bail!("refused: the diff is {size} bytes; a proposal is at most {MAX_DIFF}");
        }
        let mut diff = String::new();
        std::io::Read::read_to_string(&mut std::io::Read::take(fs::File::open(diff_path)?, MAX_DIFF + 1), &mut diff)
            .with_context(|| format!("reading diff {}", diff_path.display()))?;
        if actor == Actor::Agent && diff.len() as u64 > AGENT_MAX_DIFF {
            bail!("refused: the diff is {} bytes; the agent's are at most {AGENT_MAX_DIFF}", diff.len());
        }
        // A diff begins with one: text before its first header is no part of the change, git passes it
        // by, and what it is for is to be kept (the audit of M3, M4).
        if !(diff.starts_with("diff --git ") || diff.starts_with("--- ")) {
            bail!("refused: a diff begins with its first file's header (diff --git or ---), nothing before it");
        }
        if diff.len() as u64 > MAX_DIFF {
            bail!("refused: the diff grew past {MAX_DIFF} bytes while it was read");
        }
        // Classify exactly the bytes that go into the ledger: the caller's file is read once and
        // could change between two reads.
        let copy = self.state_dir.join("proposing.diff");
        fs::write(&copy, &diff)?;
        let checked = self.repo.diff_files(&copy).and_then(|files| {
            self.check_user_data(&copy, &files)?;
            // The agent's proposals: only what its list allows (ADR-041), judged by what changes.
            let refused = if actor == Actor::Agent { self.agent_refusal(&copy, &files)? } else { None };
            // What it does, as git writes it: what a person is shown before applying it.
            let effect = self.repo.proposal_effect(&copy)?;
            Ok((files, refused, effect))
        });
        let _ = fs::remove_file(&copy);
        let (files, refused, effect) = checked?;
        if files.is_empty() {
            bail!("diff changes no files");
        }
        // What no one's proposal may touch (the policy, the ledger) is refused as that first: the
        // agent's list comes after.
        let tier = policy::tier_for_paths(&files);
        policy::check_allowed(tier)?;
        if let Some(why) = refused {
            bail!("refused: {why}");
        }
        self.ledger.append(NewEntry {
            actor: actor.to_string(),
            approved_by: None,
            kind: "proposal".into(),
            tier: tier.to_string(),
            tainted,
            reason: reason.into(),
            data: json!({
                "files": files,
                "diff_sha256": self.keep_proposal_text(&diff)?,
                "effect_sha256": self.keep_proposal_text(&effect)?,
                "base": base,
            }),
        })
    }

    /// The config repo must be at the last ledgered commit before anything builds on it: a commit made
    /// outside the broker would otherwise become the base of an approved change, and so part of the
    /// history, though nobody reviewed or ledgered it (audit of 2026-10-31).
    fn refuse_unledgered_head(&self) -> Result<()> {
        let entries = self.ledger.read_all()?;
        let (Some(ledgered), Ok(head)) = (ledgered_commit(&entries), self.repo.head()) else { return Ok(()) };
        if head != ledgered {
            bail!(
                "refused: the config repo is at {head}, not the last ledgered commit {ledgered}: it was changed \
                 outside the broker. Nothing builds on an unledgered commit: return it to {ledgered} (as root: \
                 git -C {} reset --hard {ledgered}), then propose that change through the broker",
                self.repo.root().display()
            );
        }
        Ok(())
    }

    fn refuse_stale_base(&self, base: &str) -> Result<()> {
        let head = self.repo.head()?;
        if head != base {
            bail!(
                "refused: the configuration has changed since this change was prepared (prepared on \
                 {base}, now at {head}); prepare it again from the current configuration"
            );
        }
        Ok(())
    }

    fn find_child(&self, kind: &str, key: &str, seq: u64) -> Result<Option<Entry>> {
        Ok(self
            .ledger
            .read_all()?
            .into_iter()
            .find(|e| e.body.kind == kind && e.body.data.get(key).and_then(|v| v.as_u64()) == Some(seq)))
    }

    fn pending_path(&self) -> PathBuf {
        self.state_dir.join("pending.json")
    }

    fn read_pending(&self) -> Result<Option<Pending>> {
        match fs::read(self.pending_path()) {
            Ok(b) => {
                let p: Pending = serde_json::from_slice(&b).context("reading pending.json")?;
                if p.version > PENDING_VERSION {
                    bail!(
                        "pending.json was written by a newer broker (format {}, this one reads up to {PENDING_VERSION}); \
                         finish it with that broker",
                        p.version
                    );
                }
                Ok(Some(p))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically replace pending.json.
    fn write_pending(&self, p: &Pending) -> Result<()> {
        self.write_record(&self.pending_path(), &serde_json::to_vec_pretty(p)?)
    }

    /// Atomically replace a record in the state folder: write, fsync, rename, fsync the directory.
    fn write_record(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.state_dir)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        sync_dir(&self.state_dir)
    }

    fn setup_path(&self) -> PathBuf {
        self.state_dir.join("setup.json")
    }

    fn setup_record(&self) -> Result<Option<SetupRecord>> {
        match fs::read(self.setup_path()) {
            Ok(b) => Ok(Some(serde_json::from_slice(&b).context("reading setup.json")?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn write_setup(&self, r: &SetupRecord) -> Result<()> {
        self.write_record(&self.setup_path(), &serde_json::to_vec_pretty(r)?)
    }

    /// Setup settled (set up, failed, or finished at the next start): its build no longer needs a
    /// root of its own either.
    fn clear_setup(&self) -> Result<()> {
        self.drop_build_link();
        fs::remove_file(self.setup_path())?;
        sync_dir(&self.state_dir)
    }

    /// Finish a first-boot setup cut short after its switch: the machine runs (and boots) the new
    /// system, and its history must name it — change #1, the running system recorded as setup
    /// would have (`status` shows any difference; `repair` returns the machine to it). Only setup's
    /// own history is taken over: its one commit, the one it recorded. Nothing recorded: None.
    fn finish_setup(&self, _lock: &Lock) -> Result<Option<Entry>> {
        let Some(record) = self.setup_record()? else { return Ok(None) };
        self.ledger.verify().context("refused: the ledger is broken (`torq-broker verify`); nothing was changed")?;
        // Ledgered already: the broker died between its entry and removing the record.
        if self.ledger.read_all()?.iter().any(|e| e.body.kind == "init") {
            self.clear_setup()?;
            return Ok(None);
        }
        let head = self
            .repo
            .head()
            .context("first-boot setup was cut short before its configuration was committed: set the machine up again from the TorqOS image")?;
        if self.repo.commit_count()? != 1 || record.commit.as_ref().is_some_and(|c| *c != head) {
            bail!("first-boot setup was cut short, and the config repo now holds more than setup's commit (HEAD {head}): not taken over");
        }
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: None,
            kind: "init".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: "TorqOS set up (finished at the next start: setup was cut short after its switch)".into(),
            data: self.with_system(json!({ "commit": head, "interrupted": true })),
        })?;
        self.clear_setup()?;
        Ok(Some(e))
    }

    fn clear_pending(&self) -> Result<()> {
        fs::remove_file(self.pending_path())?;
        sync_dir(&self.state_dir)
    }

    /// Whether the ledger already holds this operation's outcome (the broker died after
    /// ledgering it but before removing pending.json).
    fn outcome_recorded(&self, p: &Pending) -> Result<bool> {
        let (kinds, key) = p.outcome()?;
        Ok(self.ledger.read_all()?.iter().skip(p.ledger_len).any(|e| {
            kinds.contains(&e.body.kind.as_str()) && e.body.data.get(key).and_then(|v| v.as_u64()) == Some(p.target)
        }))
    }

    /// For operations that must not rebuild (init, propose — the agent's entry point): settle a
    /// record whose outcome was ledgered, and refuse while one still needs reconciling.
    fn refuse_if_interrupted(&self) -> Result<()> {
        if self.setup_record()?.is_some() {
            bail!("first-boot setup was cut short: `torq-broker init` finishes it (as the machine's next start does)");
        }
        if let Some(p) = self.read_pending()? {
            // Only trust an outcome entry on an intact chain.
            self.ledger.verify().context("cannot settle an interrupted operation on a broken ledger")?;
            if !self.outcome_recorded(&p)? {
                bail!(
                    "the {} of #{} was interrupted and must be reconciled first: run `torq-broker \
                     reconcile --approved-by <name>` (it resets the config to the last ledgered commit \
                     and rebuilds)",
                    p.op, p.target
                );
            }
            self.clear_pending()?;
        }
        Ok(())
    }

    /// Finish an apply or revert whose broker died: discard its unledgered commit (kept under
    /// `refs/torq/discarded/`), reset to the last ledgered commit, rebuild, and ledger it.
    /// `by` is who ran it (the approver of the operation that triggered it, or of `reconcile`).
    pub fn reconcile(&self, by: &str) -> Result<Option<Entry>> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, by)
    }

    fn reconcile_locked(&self, lock: &Lock, by: &str) -> Result<Option<Entry>> {
        // First, whether or not anything is pending: every operation that changes the machine (apply,
        // revert, confirm, repair, expire-confirm) starts here, and on a broken chain it must change
        // nothing — its final append would fail only after it had committed, rebuilt or activated
        // (audit of 2026-10-31). An outcome entry found on a broken chain is not trustworthy either.
        self.ledger.verify().context("refused: the ledger is broken (`torq-broker verify`); nothing was changed")?;
        // A setup cut short after its switch is finished before anything else (it rebuilds nothing).
        if let Some(e) = self.finish_setup(lock)? {
            return Ok(Some(e));
        }
        let Some(mut p) = self.read_pending()? else { return Ok(None) };
        if self.outcome_recorded(&p)? {
            self.clear_pending()?;
            return Ok(None);
        }
        // Fails open if systemctl is unavailable: broker rebuilds are covered by the lock anyway.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then retry");
        }
        let index_lock = self.repo.index_lock();
        if index_lock.exists() {
            let h = holders(&index_lock);
            if !h.is_empty() {
                bail!("git is still running on the config repo ({}); retry when it exits", h.join(", "));
            }
            self.repo.remove_stale_index_lock()?;
        }
        // An interrupted rollback of an unconfirmed commit is finished, not undone: it goes to the
        // anchor recorded in the ledger, which a generic reset-and-rebuild would not ledger as such.
        if p.op == "expire" {
            return self.expire_locked(lock, Some(p.target), false, Some(p));
        }
        let head = self.repo.head()?;
        // Record the commit before resetting, so a crash in here cannot lose it from the audit.
        if p.commit.is_none() && head != p.before {
            p.commit = Some(head.clone());
            self.write_pending(&p)?;
        }
        if let Some(c) = &p.commit {
            self.repo.keep(c)?;
        }
        if !self.repo.is_ancestor(&p.before, &head)? {
            bail!(
                "config HEAD {head} does not descend from {}, the commit before the interrupted {}; \
                 refusing to reset",
                p.before, p.op
            );
        }
        // The reset destroys uncommitted changes — a half-applied diff, or someone's edit made
        // outside the broker. Keep them too.
        let uncommitted = self.repo.snapshot_dirty()?;
        if let Some(s) = &uncommitted {
            self.repo.keep(s)?;
        }
        // Always restore, even when nothing was committed: the broker may have died inside a
        // restore, after a failed rebuild had already activated the new system. An interrupted
        // commit confirmed goes straight back to its anchor system (already built, and the boot
        // default); anything else rebuilds the old config.
        let restored = match &p.anchor_system {
            Some(system) => self.restore_anchor(lock, &p.before, system)?,
            None => self.restore(&p.before, lock)?,
        };
        let e = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: Some(p.approved_by.clone()),
            kind: "interrupted".into(),
            tier: p.tier.clone(),
            tainted: p.tainted,
            reason: if restored {
                format!("Interrupted {} of #{}: config reset to {} and rebuilt", p.op, p.target, p.before)
            } else {
                format!(
                    "Interrupted {} of #{}: config reset to {} but the restore rebuild ALSO failed — \
                     running system may not match the config",
                    p.op, p.target, p.before
                )
            },
            data: self.with_system(json!({
                "op": p.op,
                "target": p.target,
                "discarded_commit": p.commit,
                "discarded_uncommitted": uncommitted,
                "commit": p.before,
                "restored": restored,
                "reconciled_by": by,
            })),
        })?;
        self.clear_pending()?;
        // An interrupted commit confirmed is settled now: its build needs no root of its own (audit).
        if p.anchor_system.is_some() {
            self.drop_build_link();
        }
        if !restored {
            bail!(
                "reconciled the interrupted {} of #{}, but the restore rebuild failed; the running \
                 system may not match the config",
                p.op, p.target
            );
        }
        Ok(Some(e))
    }

    /// The commit step failed. Nothing was rebuilt or ledgered, but `git apply --index` may have
    /// left the tree dirty (or the commit landed and only its sync failed): put the repo back and
    /// drop the record. If even that fails, the record stays and the next operation reconciles.
    fn undo_failed_commit(&self, p: &Pending, r: Result<String>) -> Result<String> {
        // A commit git made before the failure (in its sync, say) is kept for the audit, as
        // reconcile keeps one, before the reset discards it (audit of 2026-10-31).
        let keep = || -> Result<()> {
            match self.repo.head() {
                Ok(h) if h != p.before => self.repo.keep(&h),
                _ => Ok(()),
            }
        };
        r.or_else(|e| match keep().and_then(|()| self.repo.hard_reset(&p.before)).and_then(|()| self.clear_pending()) {
            Ok(()) => Err(e),
            Err(u) => Err(e.context(format!("undoing it also failed ({u:#}); the next operation will reconcile"))),
        })
    }

    /// Rebuild and switch to `rev`: the commit the caller made or restored, given rather than read
    /// again from HEAD, so not even an outside commit in between could be built in its place.
    fn rebuild(&self, lock: &Lock, rev: &str) -> Result<()> {
        let mut cmd = self.rebuild_cmd.replace("{repo}", &self.repo.root().to_string_lossy());
        if cmd.contains("{flake}") {
            cmd = cmd.replace("{flake}", &shell_quote(&self.flake_at(rev)?));
        }
        self.run_shell(lock, "rebuild", &cmd)
    }

    /// The configuration to build: the config repo at commit `rev` (`{flake}`), not its working tree —
    /// so what is built is exactly a commit the ledger names, by construction rather than by "only
    /// the broker writes there" (KNOWN-GAPS 5; second audit, H4).
    fn flake_at(&self, rev: &str) -> Result<String> {
        let root = self.repo.root().to_string_lossy();
        // A URL is built from it: an absolute path of plain characters only, a full commit id.
        let plain = root.starts_with('/') && root.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
        if !plain {
            bail!("the config repo's path {root} is not an absolute path of plain characters (needed to build it by commit)");
        }
        if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            bail!("not a commit id to build: {rev:?}");
        }
        Ok(format!("git+file://{root}?rev={rev}"))
    }

    /// Run one of the configured commands with its placeholders replaced by shell-quoted values.
    fn run_command(&self, lock: &Lock, what: &str, template: &str, vars: &[(&str, &str)]) -> Result<()> {
        self.run_command_in(lock, what, template, vars, None)
    }

    /// The same with another PATH (the anchor system's, for its restore).
    fn run_command_in(&self, lock: &Lock, what: &str, template: &str, vars: &[(&str, &str)], path: Option<&str>) -> Result<()> {
        let cmd = fill(template, vars);
        let mut c = self.shell(lock, &cmd)?;
        if let Some(p) = path {
            c.env("PATH", p);
        }
        // stdout carries the broker's result (one JSON line with --json): the command's own output
        // (activation scripts print to stdout via systemd-run --pipe) goes to stderr.
        let status = c
            .stdout(Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?))
            .status()
            .with_context(|| format!("running {what}"))?;
        if !status.success() {
            bail!("{what} command failed ({status}): {cmd}");
        }
        Ok(())
    }

    /// The same, returning what the command prints on stdout.
    fn run_command_output(&self, lock: &Lock, what: &str, template: &str, vars: &[(&str, &str)]) -> Result<String> {
        let cmd = fill(template, vars);
        let out = self.shell(lock, &cmd)?.stderr(Stdio::inherit()).output().with_context(|| format!("running {what}"))?;
        if !out.status.success() {
            bail!("{what} command failed ({}): {cmd}", out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn run_shell(&self, lock: &Lock, what: &str, cmd: &str) -> Result<()> {
        self.run_command_in(lock, what, cmd, &[], None)
    }

    /// `sh -c cmd` holding the broker lock through its stdin (see `Lock::for_child`).
    fn shell(&self, lock: &Lock, cmd: &str) -> Result<Command> {
        let mut c = config::command("sh");
        c.arg("-c").arg(cmd).stdin(lock.for_child()?);
        Ok(c)
    }

    /// Reset the config to `before` and rebuild it. A failed rebuild may already have activated
    /// the new system (switch-to-configuration exits non-zero when a unit fails), so resetting
    /// the repo alone would leave the running system out of step with it. Returns whether the
    /// restore rebuild succeeded.
    fn restore(&self, before: &str, lock: &Lock) -> Result<bool> {
        self.repo.hard_reset(before)?;
        Ok(self.rebuild(lock, before).is_ok())
    }

    fn failure_reason(what: &str, before: &str, restored: bool) -> String {
        if restored {
            format!("{what} failed; config reset to {before} and rebuilt")
        } else {
            format!("{what} failed; config reset to {before} but the restore rebuild ALSO failed — running system may not match the config")
        }
    }

    pub fn apply(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool) -> Result<Entry> {
        self.apply_inner(proposal_seq, approved_by, confirm_tainted, None)
    }

    /// `commit confirmed`: apply, but activate the new system without making it the boot default,
    /// and roll it back unless `confirm` runs within `minutes`. The rollback is a transient timer
    /// running this broker's `expire-confirm`; a reboot rolls back by itself (the boot default is
    /// still the old system) and the boot-time check ledgers it.
    pub fn apply_confirmed(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool, minutes: u32) -> Result<Entry> {
        if !(1..=MAX_CONFIRM_MINUTES).contains(&minutes) {
            bail!("commit confirmed takes 1 to {MAX_CONFIRM_MINUTES} minutes, not {minutes}");
        }
        self.apply_inner(proposal_seq, approved_by, confirm_tainted, Some(minutes))
    }

    /// While a `commit confirmed` waits, nothing else may change the machine: the rollback goes
    /// back to the configuration before it, and would silently take later changes with it.
    fn refuse_if_confirm_pending(&self) -> Result<()> {
        if let Some(pc) = pending_confirm(&self.ledger.read_all()?) {
            bail!(
                "refused: change #{} (commit confirmed) is waiting for confirmation until {}: \
                 `torq-broker confirm` keeps it, `torq-broker expire-confirm` rolls it back now",
                pc.applied, pc.deadline
            );
        }
        Ok(())
    }

    fn apply_inner(&self, proposal_seq: u64, approved_by: &str, confirm_tainted: bool, confirm: Option<u32>) -> Result<Entry> {
        if !policy::user_name_ok(approved_by) {
            bail!("{approved_by:?} is not a person's name");
        }
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        self.refuse_unledgered_head()?;
        // An outside `nixos-rebuild` switching now would interleave with this one, and the system
        // recorded after it might be that one's (as repair and reconcile check; audit of 2026-10-31).
        if switch_running() {
            bail!("refused: a system switch ({SWITCH_UNIT}) is running outside the broker; wait for it to finish, then retry");
        }
        self.refuse_if_confirm_pending()?;
        // The rollback of an unconfirmed commit — and a reboot — go back to the running system, which
        // must therefore be the boot default too (that also keeps it from garbage collection).
        let anchor_system = match confirm {
            None => None,
            Some(_) => {
                let running = self.running_system().context("commit confirmed: cannot tell which system is running")?;
                // Known, not assumed: an unreadable profile could not prove a reboot rolls back (audit).
                match self.next_boot() {
                    Some(next) if next == running => {}
                    Some(next) => bail!(
                        "commit confirmed: the next boot would start {next}, not the running system {running}, \
                         so a reboot would not roll back to it; `torq-broker repair` first"
                    ),
                    None => bail!(
                        "commit confirmed: cannot tell what the next boot starts ({}), so not that a reboot \
                         would roll back; a plain commit still works",
                        self.system_profile.display()
                    ),
                }
                // Without it, the boot-time check could not tell a restart from the switch that adds it.
                if self.boot_id().is_none() {
                    bail!("commit confirmed: cannot read the boot ID ({})", self.boot_id_file.display());
                }
                Some(running)
            }
        };
        let p = self.ledger.get(proposal_seq)?;
        if p.body.kind != "proposal" {
            bail!("entry {proposal_seq} is a '{}', not a proposal", p.body.kind);
        }
        if self.find_child("applied", "proposal", proposal_seq)?.is_some() {
            bail!("proposal {proposal_seq} was already applied");
        }
        if p.body.tainted && !confirm_tainted {
            bail!(
                "proposal {proposal_seq} came from a session that read untrusted content; \
                 re-run with explicit per-step confirmation (--confirm-tainted)"
            );
        }
        let diff = self.proposal_text(&p, "diff")?;
        if let Some(base) = p.body.data["base"].as_str() {
            self.refuse_stale_base(base)?;
        }

        // Re-validate against the current repo state (it may have changed since proposing).
        // Gone however this ends (the review of 0.2.1, L1).
        let tmp = DropFile(self.state_dir.join(format!("proposal-{proposal_seq}.diff")));
        let tmp = &tmp.0;
        fs::write(tmp, &diff)?;
        let files = self.repo.diff_files(&tmp)?;
        self.check_user_data(&tmp, &files)?;
        let tier = policy::tier_for_paths(&files);
        let actor = if p.body.actor == "agent" { Actor::Agent } else { Actor::User };
        policy::check_allowed(tier)?;
        // What was judged is what is applied. The gate decided on the proposal's tier and files, as they
        // were on the commit it was made on, and a diff means what it does at HEAD: nothing there can be
        // something here (the audit of M3, H1). Refused, never judged again: a proposal is to be made anew.
        let said: std::collections::BTreeSet<&str> = p.body.data["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).collect();
        let does: std::collections::BTreeSet<&str> = files.iter().map(String::as_str).collect();
        if said != does || p.body.tier != tier.to_string() {
            let _ = fs::remove_file(&tmp);
            let why = format!("proposal {proposal_seq} no longer does what it was judged on: it said {} ({}), and would now change {} ({tier}); propose it again", said.into_iter().collect::<Vec<_>>().join(" "), p.body.tier, does.into_iter().collect::<Vec<_>>().join(" "));
            // Once each: a person repeating this adds nothing to the record.
            if self.find_child("apply_refused", "proposal", proposal_seq)?.is_none() {
                self.ledger.append(NewEntry {
                    actor: "broker".into(),
                    approved_by: Some(approved_by.into()),
                    kind: "apply_refused".into(),
                    tier: Tier::System.to_string(),
                    tainted: false,
                    reason: why.chars().filter(|c| !c.is_control()).take(300).collect(),
                    data: json!({ "proposal": proposal_seq }),
                })?;
            }
            bail!("refused: {why}");
        }
        // The agent's list as it is now, not as it was when it proposed: the limits may have been
        // narrowed since (the audit of ADR-041).
        if actor == Actor::Agent {
            if let Some(why) = self.agent_refusal(&tmp, &files)? {
                let _ = fs::remove_file(&tmp);
                bail!("refused: the agent's limits no longer allow this — {why}");
            }
        }

        let next_seq = self.ledger.read_all()?.len() as u64 + 1;
        let msg = format!(
            "{}\n\nActor: {}\nApproved-By: {approved_by}\nTier: {tier}\nProposal: {proposal_seq}\nLedger-Entry: {next_seq}",
            p.body.reason, p.body.actor
        );
        let before = self.repo.head()?;
        self.repo.ensure_clean()?; // before recording anything: a failed commit is undone by a reset
        let mut pending = Pending {
            version: PENDING_VERSION,
            op: "apply".into(),
            target: proposal_seq,
            before: before.clone(),
            ledger_len: (next_seq - 1) as usize,
            commit: None,
            approved_by: approved_by.into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
            anchor_system: anchor_system.clone(),
            attempts: 0,
        };
        self.write_pending(&pending)?;
        let commit = self.undo_failed_commit(&pending, self.repo.apply_and_commit(&tmp, &msg))?;
        let _ = fs::remove_file(&tmp);
        pending.commit = Some(commit.clone());
        let snapshot = self.take_snapshot(&lock, &tier.to_string());
        self.write_pending(&pending)?;

        // Plain: rebuild and switch. Confirmed: build, schedule the rollback, and only then activate
        // (without touching the boot default) — so no moment has the new system running without a
        // timer to undo it. A failure in any step goes straight back to the anchor, not a rebuild;
        // a broker killed in any step is reconciled the same way (`pending.anchor_system`), by the
        // next operation or by the timer.
        // This attempt's own, in milliseconds: never a ledger number, which `torq break-glass` could take
        // while the build runs, and never an earlier attempt's, whose timer may still be waiting.
        let timer = chrono::Utc::now().timestamp_millis().unsigned_abs();
        let activated = match (confirm, &anchor_system) {
            (Some(minutes), Some(anchor)) => self
                .build_system(&lock, &commit)
                .and_then(|system| {
                    let deadline = self.schedule_rollback(&lock, timer, minutes)?;
                    self.run_command(&lock, "activation", &self.activate_cmd, &[("system", &system)])?;
                    if self.running_system().as_deref() != Some(system.as_str()) {
                        bail!("activation finished, but the running system is not {system}");
                    }
                    Ok(deadline)
                })
                .map(|deadline| Some(json!({
                    "timer": timer,
                    "minutes": minutes,
                    "deadline": deadline,
                    "anchor_commit": before,
                    "anchor_system": anchor,
                    "boot_id": self.boot_id(),
                })))
                .map_err(|e| (e, Some(anchor))),
            _ => self.rebuild(&lock, &commit).map(|()| None).map_err(|e| (e, None)),
        };
        let confirm_record = match activated {
            Ok(c) => c,
            Err((e, anchor)) => {
                let restored = match anchor {
                    Some(system) => self.restore_anchor(&lock, &before, system)?,
                    None => self.restore(&before, &lock)?,
                };
                self.ledger.append(NewEntry {
                    actor: "broker".into(),
                    approved_by: Some(approved_by.into()),
                    kind: "apply_failed".into(),
                    tier: tier.to_string(),
                    tainted: p.body.tainted,
                    reason: Self::failure_reason(if anchor.is_some() { "Commit confirmed" } else { "Rebuild" }, &before, restored),
                    data: self.with_system(with_snapshot(json!({ "proposal": proposal_seq, "error": e.to_string(), "restored": restored }), &snapshot)),
                })?;
                self.clear_pending()?;
                self.drop_build_link();
                let what = if anchor.is_some() { "commit confirmed failed" } else { "the rebuild failed" };
                return Err(e.context(format!("apply undone because {what}")));
            }
        };

        let mut data = with_snapshot(json!({ "proposal": proposal_seq, "commit": commit, "files": files }), &snapshot);
        if let Some(c) = confirm_record {
            data["confirm"] = c;
        }
        let e = self.ledger.append(NewEntry {
            actor: p.body.actor.clone(),
            approved_by: Some(approved_by.into()),
            kind: "applied".into(),
            tier: tier.to_string(),
            tainted: p.body.tainted,
            reason: p.body.reason.clone(),
            data: self.with_system(data),
        })?;
        self.clear_pending()?;
        Ok(e)
    }

    /// Build the committed config without activating it; returns the system's store path (the last
    /// line the build command prints). It stays a GC root (`state/confirm-system`) until the change
    /// settles: a garbage collection between build and activation must not delete it.
    fn build_system(&self, lock: &Lock, rev: &str) -> Result<String> {
        let link = self.build_link();
        let flake = if self.build_cmd.contains("{flake}") { self.flake_at(rev)? } else { String::new() };
        let vars = [("repo", &*self.repo.root().to_string_lossy()), ("flake", flake.as_str()), ("link", &*link.to_string_lossy())];
        let out = self.run_command_output(lock, "build", &self.build_cmd, &vars)?;
        let system = out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_string();
        if !system.starts_with('/') {
            bail!("the build printed no system path (got {:?})", out.trim());
        }
        Ok(system)
    }

    fn build_link(&self) -> PathBuf {
        self.state_dir.join("confirm-system")
    }

    /// The change settled (confirmed, rolled back, or failed): its build no longer needs a root of
    /// its own (a confirmed one is the boot default now).
    fn drop_build_link(&self) {
        let _ = fs::remove_file(self.build_link());
    }

    /// Start the timer that rolls back this attempt's change after `minutes`; returns the deadline.
    /// `timer` is what it will name: the `timer` of the `confirm` record.
    /// It runs this broker by its own path (`exe`), so the rollback does not depend on the system
    /// being rolled back. If this broker dies before the change is ledgered, the timer's run reconciles
    /// the interrupted apply (`pending.anchor_system`): the anchor is restored all the same.
    fn schedule_rollback(&self, lock: &Lock, timer: u64, minutes: u32) -> Result<String> {
        let deadline = chrono::Utc::now() + chrono::Duration::minutes(minutes.into());
        self.run_command(
            lock,
            "rollback timer",
            &self.schedule_cmd,
            &[
                ("seq", &timer.to_string()),
                ("seconds", &(u64::from(minutes) * 60).to_string()),
                ("deadline", &deadline.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                ("broker", &self.exe.to_string_lossy()),
            ],
        )?;
        Ok(deadline.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
    }

    /// Go back to `commit` and the already-built `system` without rebuilding: reset the config,
    /// activate the system unless it is already running, and make it the boot default if something
    /// moved that. Success is what the machine ended up as — running `system`, with it as the boot
    /// default — not the commands' exit codes (switch-to-configuration exits non-zero when any unit
    /// fails to start, after switching). On a real machine the commands and PATH are the anchor
    /// system's own (`anchor_settings`): the change being undone may have broken this system's.
    fn restore_anchor(&self, lock: &Lock, commit: &str, system: &str) -> Result<bool> {
        self.repo.hard_reset(commit)?;
        let (activate, boot, path) = self.restore_commands(system);
        let path = path.as_deref();
        if self.running_system().as_deref() != Some(system) {
            let _ = self.run_command_in(lock, "activation", &activate, &[("system", system)], path);
        }
        if self.next_boot().as_deref() != Some(system) {
            let profile = self.system_profile.to_string_lossy();
            let _ = self.run_command_in(lock, "boot default", &boot, &[("system", system), ("profile", &profile)], path);
        }
        // Restored only if both are known to be it: an unreadable profile is not "restored" (audit).
        Ok(self.running_system().as_deref() == Some(system) && self.next_boot().as_deref() == Some(system))
    }

    /// The activate and boot commands, and the PATH, to restore `system` with.
    fn restore_commands(&self, system: &str) -> (String, String, Option<String>) {
        if self.anchor_settings {
            let file = Path::new(system).join("etc/torq/broker.json");
            match config::Settings::read(&file, true) {
                Ok(Some(s)) => return (s.activate, s.boot, Some(s.path)),
                Ok(None) => eprintln!("note: {} is missing; restoring with this system's commands", file.display()),
                Err(e) => eprintln!("note: {e:#}; restoring with this system's commands"),
            }
        }
        (self.activate_cmd.clone(), self.boot_cmd.clone(), None)
    }

    /// Keep the commit confirmed that is waiting: make the system it activated the boot default and
    /// ledger `confirmed`. Accepted until its rollback has started (the rollback takes the lock),
    /// not by the clock. It must be the running system: after a reboot or a switch elsewhere there
    /// is nothing to keep.
    pub fn confirm(&self, by: &str) -> Result<Entry> {
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, by)?;
        let Some(pc) = pending_confirm(&self.ledger.read_all()?) else {
            bail!("nothing is waiting for confirmation");
        };
        // Only the one who made it keeps it: someone else must not decide a change is fine before
        // its author has checked it. (Who is calling is still the caller's word: KNOWN-GAPS #4.)
        if pc.approved_by.as_deref().is_some_and(|who| who != by) {
            bail!(
                "change #{} is waiting for {} to confirm it; it rolls back at {} unless they do",
                pc.applied,
                pc.approved_by.as_deref().unwrap_or("?"),
                pc.deadline
            );
        }
        if pc.boot_id.is_some() && pc.boot_id != self.boot_id() {
            bail!("change #{} cannot be confirmed: the machine has restarted since it was applied; it will be rolled back", pc.applied);
        }
        let running = self.running_system();
        let Some(system) = pc.system.clone().filter(|s| running.as_ref() == Some(s)) else {
            bail!(
                "change #{} cannot be confirmed: the running system is {}, not the one it activated ({}); \
                 it will be rolled back",
                pc.applied,
                running.as_deref().unwrap_or("(unknown)"),
                pc.system.as_deref().unwrap_or("(unknown)")
            );
        };
        self.run_command(&lock, "boot default", &self.boot_cmd, &[("system", &system), ("profile", &self.system_profile.to_string_lossy())])
            .with_context(|| format!("change #{} is still unconfirmed and will be rolled back at {}", pc.applied, pc.deadline))?;
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "confirmed".into(),
            tier: pc.tier.clone(),
            tainted: false,
            reason: format!("Confirmed: {}", pc.reason),
            data: self.with_system(json!({ "applied": pc.applied })),
        })
        .inspect(|_| self.drop_build_link())
    }

    /// Roll back the commit confirmed that is waiting (`seq`, if given, must be it): what its timer
    /// runs at the deadline, the boot-time check after a reboot (`by_reboot`), or a person who
    /// wants it gone now. Nothing waiting is not an error: a confirmed commit's timer still fires.
    pub fn expire_confirm(&self, seq: Option<u64>, by_reboot: bool) -> Result<Option<Entry>> {
        let lock = Lock::acquire(&self.state_dir)?;
        if let Some(e) = self.reconcile_locked(&lock, "broker")? {
            if e.body.kind == "rolled_back" {
                return Ok(Some(e)); // it was this, interrupted: now finished
            }
        }
        self.expire_locked(&lock, seq, by_reboot, None)
    }

    /// Idempotent, so an interrupted rollback is finished by running it again (`resumed`).
    fn expire_locked(&self, lock: &Lock, seq: Option<u64>, by_reboot: bool, resumed: Option<Pending>) -> Result<Option<Entry>> {
        let entries = self.ledger.read_all()?;
        let pc = pending_confirm(&entries).filter(|pc| seq.is_none_or(|s| s == pc.applied || Some(s) == pc.timer));
        // "By reboot" only after one: the boot-time unit also starts on the switch that first adds it.
        let pc = pc.filter(|pc| !by_reboot || pc.boot_id.is_none() || pc.boot_id != self.boot_id());
        let Some(pc) = pc else {
            if resumed.is_some() {
                self.clear_pending()?;
            }
            return Ok(None);
        };
        self.ledger.verify().context("cannot roll back on a broken ledger")?;
        // A manual nixos-rebuild does not take the broker lock. Failing makes the timer retry.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; the rollback waits for it");
        }
        let head = self.repo.head()?;
        let (running_before, next_before) = (self.running_system(), self.next_boot());
        let mut p = match resumed {
            Some(p) => p,
            None => Pending {
                version: PENDING_VERSION,
                op: "expire".into(),
                target: pc.applied,
                before: head.clone(),
                ledger_len: entries.len(),
                commit: None,
                approved_by: "broker".into(),
                tier: pc.tier.clone(),
                tainted: false,
                anchor_system: Some(pc.anchor_system.clone()),
                attempts: 0,
            },
        };
        // What the reset discards is kept (refs/torq/discarded/): the unconfirmed commit, and anything
        // changed behind the broker's back since.
        if head != pc.anchor_commit {
            p.commit = Some(head.clone());
            self.repo.keep(&head)?;
        }
        self.write_pending(&p)?;
        let uncommitted = self.repo.snapshot_dirty()?;
        if let Some(s) = &uncommitted {
            self.repo.keep(s)?;
        }
        // Not done until the machine is back: the operation stays pending (and the change unsettled),
        // so the timer's retries, the next operation, or the next boot finish it. But not for ever:
        // while it is pending every other operation stops on it. After MAX_ROLLBACK_ATTEMPTS it is
        // ledgered as not restored and settled; `status` reports it, `repair` (or a reboot) fixes it.
        let restored = self.restore_anchor(lock, &pc.anchor_commit, &pc.anchor_system)?;
        if !restored {
            p.attempts += 1;
            let found = format!(
                "running {}, next boot {}; want {}",
                self.running_system().as_deref().unwrap_or("?"),
                self.next_boot().as_deref().unwrap_or("?"),
                pc.anchor_system
            );
            if p.attempts < MAX_ROLLBACK_ATTEMPTS {
                self.write_pending(&p)?;
                bail!(
                    "the rollback of change #{} did not take ({found}); attempt {} of {MAX_ROLLBACK_ATTEMPTS}, it \
                     is retried — a reboot also rolls it back",
                    pc.applied, p.attempts
                );
            }
        }
        // Whatever ran it (a timer retry or reconcile can run after a restart too), a new boot ID
        // means the machine restarted while the change waited.
        let by_reboot = by_reboot || (pc.boot_id.is_some() && pc.boot_id != self.boot_id());
        let why = if by_reboot { "the machine restarted before it was confirmed" } else { "it was not confirmed in time" };
        let e = self.ledger.append(NewEntry {
            actor: "broker".into(),
            approved_by: None,
            kind: "rolled_back".into(),
            tier: pc.tier.clone(),
            tainted: false,
            reason: if restored {
                format!("Change #{} rolled back: {why}", pc.applied)
            } else {
                format!(
                    "Change #{} rolled back ({why}), but after {} attempts the old system is still not running \
                     and the boot default: reboot, or `torq-broker repair`",
                    pc.applied, p.attempts
                )
            },
            data: self.with_system(json!({
                "applied": pc.applied,
                "unconfirmed": true,
                "by_reboot": by_reboot,
                "commit": pc.anchor_commit,
                "discarded_commit": p.commit,
                "discarded_uncommitted": uncommitted,
                "restored": restored,
                "attempts": p.attempts,
                "found": { "running_system": running_before, "next_boot": next_before },
            })),
        })?;
        self.clear_pending()?;
        self.drop_build_link();
        if !restored {
            bail!("{} (recorded as #{})", e.body.reason, e.body.seq);
        }
        Ok(Some(e))
    }

    /// Undo one applied change by a new commit that reverts it. (Not `rollback`: that word is kept
    /// for loading a whole earlier configuration, and for an unconfirmed `commit confirmed`.)
    pub fn revert(&self, applied_seq: u64, approved_by: &str) -> Result<Entry> {
        if !policy::user_name_ok(approved_by) {
            bail!("{approved_by:?} is not a person's name");
        }
        let lock = Lock::acquire(&self.state_dir)?;
        self.reconcile_locked(&lock, approved_by)?;
        self.refuse_unledgered_head()?;
        // An outside `nixos-rebuild` switching now would interleave with this one, and the system
        // recorded after it might be that one's (as repair and reconcile check; audit of 2026-10-31).
        if switch_running() {
            bail!("refused: a system switch ({SWITCH_UNIT}) is running outside the broker; wait for it to finish, then retry");
        }
        self.refuse_if_confirm_pending()?;
        let a = self.ledger.get(applied_seq)?;
        if a.body.kind != "applied" {
            bail!("entry {applied_seq} is a '{}', not an applied change", a.body.kind);
        }
        if self.find_child("reverted", "applied", applied_seq)?.is_some() {
            bail!("change {applied_seq} was already reverted");
        }
        let commit = a.body.data["commit"].as_str().context("applied entry has no commit")?.to_string();
        let next_seq = self.ledger.read_all()?.len() as u64 + 1;
        let msg = format!(
            "Undo: {}\n\nReverts: {commit}\nApproved-By: {approved_by}\nLedger-Entry: {next_seq}",
            a.body.reason
        );
        let before = self.repo.head()?;
        self.repo.ensure_clean()?; // before recording anything: a failed commit is undone by a reset
        let mut pending = Pending {
            version: PENDING_VERSION,
            op: "revert".into(),
            target: applied_seq,
            before: before.clone(),
            ledger_len: (next_seq - 1) as usize,
            commit: None,
            approved_by: approved_by.into(),
            tier: a.body.tier.clone(),
            tainted: false,
            anchor_system: None,
            attempts: 0,
        };
        self.write_pending(&pending)?;
        let new_commit = self.undo_failed_commit(&pending, self.repo.revert_and_commit(&commit, &msg))?;
        pending.commit = Some(new_commit.clone());
        let snapshot = self.take_snapshot(&lock, &a.body.tier);
        self.write_pending(&pending)?;
        if let Err(e) = self.rebuild(&lock, &new_commit) {
            let restored = self.restore(&before, &lock)?;
            self.ledger.append(NewEntry {
                actor: "broker".into(),
                approved_by: Some(approved_by.into()),
                kind: "revert_failed".into(),
                tier: a.body.tier.clone(),
                tainted: false,
                reason: Self::failure_reason("Revert rebuild", &before, restored),
                data: self.with_system(with_snapshot(json!({ "applied": applied_seq, "error": e.to_string(), "restored": restored }), &snapshot)),
            })?;
            self.clear_pending()?;
            return Err(e.context("revert aborted because the rebuild failed"));
        }
        let data = with_snapshot(json!({ "applied": applied_seq, "reverted_commit": commit, "commit": new_commit }), &snapshot);
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "reverted".into(),
            tier: a.body.tier.clone(),
            tainted: false,
            reason: format!("Undo: {}", a.body.reason),
            data: self.with_system(data),
        })?;
        self.clear_pending()?;
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(pending: Option<PendingConfirm>, overdue: bool, next_boot: &str) -> Status {
        Status {
            ledger: Ok(3),
            disagreements: vec![],
            torn: false,
            store_modified: vec![],
            head: Some("c".into()),
            dirty: false,
            ledgered_commit: Some("c".into()),
            running_system: Some("/nix/store/new".into()),
            next_boot: Some(next_boot.into()),
            ledgered_system: Some("/nix/store/new".into()),
            unrestored: None,
            interrupted: None,
            busy: None,
            pending_confirm: pending,
            overdue,
            remote: None,
            remote_alarm: None,
            remote_refused: None,
        }
    }

    fn waiting() -> PendingConfirm {
        PendingConfirm {
            timer: None,
            applied: 3,
            deadline: "2026-10-30T12:00:00Z".into(),
            anchor_commit: "a".into(),
            anchor_system: "/nix/store/old".into(),
            system: Some("/nix/store/new".into()),
            boot_id: Some("b".into()),
            approved_by: Some("casey".into()),
            tier: "system".into(),
            reason: "r".into(),
        }
    }

    #[test]
    fn a_waiting_commit_confirmed_is_not_drift_until_overdue() {
        assert!(status(Some(waiting()), false, "/nix/store/old").problems().is_empty());
        let p = status(Some(waiting()), true, "/nix/store/old").problems();
        assert!(p.len() == 1 && p[0].contains("was not confirmed by 2026-10-30T12:00:00Z"), "{p:?}");
        // Only the anchor is exempt as the boot default.
        let p = status(Some(waiting()), false, "/nix/store/other").problems();
        assert!(p.len() == 1 && p[0].contains("next boot"), "{p:?}");
        let p = status(None, false, "/nix/store/old").problems();
        assert!(p.len() == 1 && p[0].contains("next boot"), "{p:?}");
    }
}
