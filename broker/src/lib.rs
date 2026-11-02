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

pub mod config;
pub mod ledger;
pub mod policy;
pub mod repo;

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
}

/// A `commit confirmed` waiting for `confirm`: an `applied` entry with a `confirm` record and no
/// `confirmed` or `rolled_back` for it. Derived from the ledger alone, never from a separate file.
#[derive(Debug, Clone, Serialize)]
pub struct PendingConfirm {
    pub applied: u64,
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
        .any(|e| matches!(e.body.kind.as_str(), "confirmed" | "rolled_back") && e.body.data["applied"] == json!(seq));
    if settled {
        return None;
    }
    let c = &a.body.data["confirm"];
    Some(PendingConfirm {
        applied: seq,
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

/// How the machine compares with its ledger. See `Broker::status`.
#[derive(Debug)]
pub struct Status {
    pub ledger: std::result::Result<usize, String>,
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
}

impl Status {
    /// Everything that is not as the ledger says, worded for a person. Empty means all is well —
    /// or that it could not be checked because an operation is running (`busy`).
    pub fn problems(&self) -> Vec<String> {
        if self.busy.is_some() {
            return Vec::new();
        }
        let mut p = Vec::new();
        if let Err(e) = &self.ledger {
            p.push(format!("ledger: {e}"));
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
                     broker (e.g. nixos-rebuild against another configuration, or an old boot entry)",
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(l) = Self::try_acquire(dir)? {
                return Ok(l);
            }
            if std::time::Instant::now() >= deadline {
                return Err(Busy(Self::busy(dir)).into());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// The lock, or None while another operation holds it.
    fn try_acquire(dir: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(dir)?;
        let p = dir.join("broker.lock");
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
    /// broker does not stop `nixos-rebuild`, which may still switch the system; the next operation
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
        .filter(|e| matches!(e.body.kind.as_str(), "init" | "applied" | "reverted" | "interrupted" | "repaired" | "rolled_back"))
        .find_map(|e| e.body.data.get("commit").and_then(|v| v.as_str()).map(String::from))
}

/// nixos-rebuild runs the switch in this transient unit (`systemd-run --pipe`). A broker rebuild's
/// switch inherits the lock through stdin, but a manual `nixos-rebuild` outside the broker does not.
const SWITCH_UNIT: &str = "nixos-rebuild-switch-to-configuration.service";

fn switch_running() -> bool {
    config::command("systemctl")
        .args(["is-active", "--quiet", SWITCH_UNIT])
        .status()
        .is_ok_and(|s| s.success())
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
            boot_cmd: config::BOOT.into(),
            schedule_cmd: config::SCHEDULE.into(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("torq-broker")),
            boot_id_file: PathBuf::from("/proc/sys/kernel/random/boot_id"),
            anchor_settings: false,
            passwords_dir: PathBuf::from("/var/lib/torq/secrets/passwords"),
            password_cmd: "chpasswd -e".into(),
            people: Vec::new(),
        }
    }

    /// The people the configuration declares (its normal users, from the broker's fixed settings).
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
    fn with_system(&self, mut data: serde_json::Value) -> serde_json::Value {
        data["system"] = self.running_system().into();
        data
    }

    /// Compare the machine with its ledger. Read-only, but under the lock: during an operation the
    /// comparison would be meaningless, so it reports `busy` instead.
    pub fn status(&self) -> Result<Status> {
        let lock = Lock::try_acquire(&self.state_dir)?; // held until the comparison is done
        let busy = lock.is_none().then(|| Lock::busy(&self.state_dir));
        self.facts(busy)
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
        if before.problems().is_empty() {
            bail!("nothing to repair: the machine is as its ledger says");
        }
        // A manual nixos-rebuild does not take the broker lock: switching now would race it, and the
        // result could be its system recorded as ours.
        if switch_running() {
            bail!("a system switch ({SWITCH_UNIT}) is still running; wait for it to finish, then repair");
        }
        let rebuilt = self.rebuild(&lock, &head).is_ok();
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
                    "problems": before.problems(),
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
    /// - Then switched to, and ledgered whatever the switch says: a switch makes the new system the
    ///   boot default before activating it, so from then on the machine starts it, and its history
    ///   must name it. A switch that reported a problem says so in the entry; `status` shows any
    ///   difference, and `repair` returns the machine to it.
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
        if let Err(e) = self.build_system(&lock, &head) {
            let _ = std::fs::remove_dir_all(self.repo.root().join(".git"));
            let _ = self.clear_setup();
            return Err(e.context("the new configuration did not build: nothing was set up"));
        }
        let switched = self.rebuild(&lock, &head);
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
        // At once: the running system's own tool. It fails for an account this machine does not have.
        let mut words = self.password_cmd.split_whitespace();
        let mut cmd = config::command(words.next().unwrap_or("chpasswd"));
        cmd.args(words).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = cmd.spawn().context("setting the password")?;
        child.stdin.take().context("setting the password")?.write_all(format!("{user}:{hash}\n").as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("the password was not set: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        // Kept: written whole (a new file, then renamed over the old), private to the system.
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.passwords_dir)?;
            let part = self.passwords_dir.join(format!(".{user}.new"));
            let _ = std::fs::remove_file(&part);
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            f.write_all(format!("{hash}\n").as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&part, self.passwords_dir.join(user))?;
        }
        self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(by.into()),
            kind: "password".into(),
            tier: Tier::System.to_string(),
            tainted: false,
            reason: format!("Password changed for {user}"),
            data: json!({ "user": user }),
        })
    }

    pub fn init(&self) -> Result<()> {
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

    pub fn propose(&self, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        self.propose_on(None, diff_path, reason, actor, tainted)
    }

    /// Propose a change prepared against the configuration at commit `base`. It is refused now if
    /// the configuration has moved on since, and again at `apply` if it moves before then: applied
    /// on top of a newer configuration, a change prepared against an older one can silently undo or
    /// mix with the commit in between. Without a base, the diff is applied wherever it applies.
    pub fn propose_on(&self, base: Option<&str>, diff_path: &Path, reason: &str, actor: Actor, tainted: bool) -> Result<Entry> {
        let _l = Lock::acquire(&self.state_dir)?;
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
        if diff.len() as u64 > MAX_DIFF {
            bail!("refused: the diff grew past {MAX_DIFF} bytes while it was read");
        }
        // Classify exactly the bytes that go into the ledger: the caller's file is read once and
        // could change between two reads.
        let copy = self.state_dir.join("proposing.diff");
        fs::write(&copy, &diff)?;
        let files = self.repo.diff_files(&copy).and_then(|files| {
            self.check_user_data(&copy, &files)?;
            Ok(files)
        });
        let _ = fs::remove_file(&copy);
        let files = files?;
        if files.is_empty() {
            bail!("diff changes no files");
        }
        let tier = policy::tier_for_paths(&files);
        policy::check_allowed(actor, tier)?;
        self.ledger.append(NewEntry {
            actor: actor.to_string(),
            approved_by: None,
            kind: "proposal".into(),
            tier: tier.to_string(),
            tainted,
            reason: reason.into(),
            data: json!({
                "files": files,
                "diff_sha256": hex::encode(Sha256::digest(diff.as_bytes())),
                "diff": diff,
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
        let diff = p.body.data["diff"].as_str().context("proposal has no diff")?;
        if let Some(base) = p.body.data["base"].as_str() {
            self.refuse_stale_base(base)?;
        }

        // Re-validate against the current repo state (it may have changed since proposing).
        let tmp = self.state_dir.join(format!("proposal-{proposal_seq}.diff"));
        fs::write(&tmp, diff)?;
        let files = self.repo.diff_files(&tmp)?;
        self.check_user_data(&tmp, &files)?;
        let tier = policy::tier_for_paths(&files);
        let actor = if p.body.actor == "agent" { Actor::Agent } else { Actor::User };
        policy::check_allowed(actor, tier)?;

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
        self.write_pending(&pending)?;

        // Plain: rebuild and switch. Confirmed: build, schedule the rollback, and only then activate
        // (without touching the boot default) — so no moment has the new system running without a
        // timer to undo it. A failure in any step goes straight back to the anchor, not a rebuild;
        // a broker killed in any step is reconciled the same way (`pending.anchor_system`), by the
        // next operation or by the timer.
        let activated = match (confirm, &anchor_system) {
            (Some(minutes), Some(anchor)) => self
                .build_system(&lock, &commit)
                .and_then(|system| {
                    let deadline = self.schedule_rollback(&lock, next_seq, minutes)?;
                    self.run_command(&lock, "activation", &self.activate_cmd, &[("system", &system)])?;
                    if self.running_system().as_deref() != Some(system.as_str()) {
                        bail!("activation finished, but the running system is not {system}");
                    }
                    Ok(deadline)
                })
                .map(|deadline| Some(json!({
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
                    data: self.with_system(json!({ "proposal": proposal_seq, "error": e.to_string(), "restored": restored })),
                })?;
                self.clear_pending()?;
                self.drop_build_link();
                let what = if anchor.is_some() { "commit confirmed failed" } else { "the rebuild failed" };
                return Err(e.context(format!("apply undone because {what}")));
            }
        };

        let mut data = json!({ "proposal": proposal_seq, "commit": commit, "files": files });
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

    /// Start the timer that rolls back applied entry `seq` after `minutes`; returns the deadline.
    /// It runs this broker by its own path (`exe`), so the rollback does not depend on the system
    /// being rolled back. If this broker dies before `seq` is ledgered, the timer's run reconciles
    /// the interrupted apply (`pending.anchor_system`): the anchor is restored all the same.
    fn schedule_rollback(&self, lock: &Lock, seq: u64, minutes: u32) -> Result<String> {
        let deadline = chrono::Utc::now() + chrono::Duration::minutes(minutes.into());
        self.run_command(
            lock,
            "rollback timer",
            &self.schedule_cmd,
            &[
                ("seq", &seq.to_string()),
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
        let pc = pending_confirm(&entries).filter(|pc| seq.is_none_or(|s| s == pc.applied));
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
                data: self.with_system(json!({ "applied": applied_seq, "error": e.to_string(), "restored": restored })),
            })?;
            self.clear_pending()?;
            return Err(e.context("revert aborted because the rebuild failed"));
        }
        let e = self.ledger.append(NewEntry {
            actor: "user".into(),
            approved_by: Some(approved_by.into()),
            kind: "reverted".into(),
            tier: a.body.tier.clone(),
            tainted: false,
            reason: format!("Undo: {}", a.body.reason),
            data: self.with_system(json!({ "applied": applied_seq, "reverted_commit": commit, "commit": new_commit })),
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
        }
    }

    fn waiting() -> PendingConfirm {
        PendingConfirm {
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
