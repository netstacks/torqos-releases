//! `configure exclusive` (docs/CLI-DESIGN.md, as on a network device): one interactive session holds
//! the shared candidate alone until it leaves configuration mode; the person's other sessions can
//! read it, not change it, and are told which session holds it. The private candidate of
//! `torqcli -c` has nothing to share and is never locked.
//!
//! The lock is the kernel's (`flock` on a file beside the candidate), held as long as the session's
//! file is open: a session that dies, however, lets go of it with its process, so a lock can never
//! outlive the process that took it, and a reused process number cannot keep it. The file says who
//! holds it (process, terminal, since when); what a session that ended without leaving wrote there
//! is a stale lock, found free and taken over, and said so.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Where the lock of a saved candidate is kept: beside it, in the same private folder.
pub fn path_for(candidate: &Path) -> PathBuf {
    candidate.with_extension("lock")
}

/// The session holding the lock, as it wrote itself into the file.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub pid: u32,
    /// The terminal it runs at (`pts/3`, `tty1`), if it has one.
    pub terminal: Option<String>,
    /// When it took the lock (UTC, as the broker writes times: `2026-11-07T14:02:51Z`).
    pub since: String,
}

impl Holder {
    /// This session, now.
    pub fn me() -> Holder {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let terminal = std::fs::read_link("/proc/self/fd/0").ok().and_then(|t| t.to_str()?.strip_prefix("/dev/").filter(|t| t.starts_with("pts/") || t.starts_with("tty")).map(String::from));
        Holder { pid: std::process::id(), terminal, since: utc(now) }
    }

    /// As others are told of it: `torqcli process 4242 on pts/3, since 2026-11-07 15:02:51`. (Read
    /// back from a file: cleaned, so nothing in it can draw on a terminal.)
    pub fn describe(&self, clock: &crate::history::Clock) -> String {
        let at = self.terminal.as_deref().map(|t| format!(" on {}", crate::history::one_line(t, 20))).unwrap_or_default();
        format!("torqcli process {}{at}, since {}", self.pid, crate::history::local_stamp(&self.since, clock))
    }

    fn render(&self) -> String {
        serde_json::json!({ "pid": self.pid, "terminal": self.terminal, "since": self.since }).to_string()
    }

    fn parse(text: &str) -> Option<Holder> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        Some(Holder {
            pid: u32::try_from(v["pid"].as_u64()?).ok()?,
            terminal: v["terminal"].as_str().map(String::from),
            since: v["since"].as_str()?.to_string(),
        })
    }
}

/// Unix seconds as the broker writes times (UTC).
fn utc(secs: u64) -> String {
    format!("{}T{:02}:{:02}:{:02}Z", crate::pkg::ymd(secs), secs % 86_400 / 3_600, secs % 3_600 / 60, secs % 60)
}

/// A lock this session holds. Dropped, it lets go: the file is emptied (no holder) and closed.
#[derive(Debug)]
pub struct Lock {
    file: File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        // (Emptied while still held, so nobody reads a holder that has let go. The kernel's lock goes
        // with the file, closed next.)
        let _ = self.file.set_len(0);
    }
}

/// The lock as another session finds it.
#[derive(Debug, Clone, PartialEq)]
pub enum Seen {
    /// Nobody holds it (whatever a session that ended without letting go wrote in it: stale).
    Free,
    /// A session holds it (who, if it could be read).
    Held(Option<Holder>),
}

/// The lock file, opened to read and write: created private (0600), never through a link, in a
/// folder only its owner can write (the candidate's: candidate.rs).
fn open(path: &Path) -> Result<File> {
    crate::candidate::private_dir(path.parent().context("the candidate's lock has no folder")?)?;
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .with_context(|| format!("opening {} (the candidate's lock; a plain file of your own)", path.display()))
}

fn written(f: &mut File) -> Result<Option<Holder>> {
    let mut text = String::new();
    f.rewind()?;
    f.read_to_string(&mut text)?;
    Ok(Holder::parse(&text))
}

/// Whether the kernel lets this file be locked now, `shared` (to look) or exclusive (to take);
/// false: someone holds it. A look never waits (only a holder's lock keeps it out); a take meeting a
/// look tries again shortly, so a session looking at the lock never keeps another from taking it.
fn try_lock(f: &File, shared: bool) -> Result<bool> {
    let tries = if shared { 1 } else { 40 };
    for n in 1..=tries {
        match if shared { f.try_lock_shared() } else { f.try_lock() } {
            Ok(()) => return Ok(true),
            Err(std::fs::TryLockError::WouldBlock) if n < tries => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking the candidate"),
        }
    }
    Ok(false)
}

/// Whether another session holds the lock file `f` (who, if it wrote itself in), else None — and then
/// this one holds it shared, until `f` is closed. Locked with nobody written in it is a moment, not
/// a holder — a session between taking and writing it, or a process just started (fork) holding a
/// copy of a session's file until it runs its program (which closes it) — so it is looked at
/// again, briefly, before it is called held by someone unknown.
fn holder_of(f: &mut File) -> Result<Option<Option<Holder>>> {
    for _ in 0..40 {
        if try_lock(f, true)? {
            return Ok(None);
        }
        if let Some(who) = written(f)? {
            return Ok(Some(Some(who)));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(Some(None))
}

/// How the lock at `path` stands, to a session that does not hold it.
pub fn seen(path: &Path) -> Result<Seen> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(Seen::Free);
    }
    Ok(holder_of(&mut open(path)?)?.map_or(Seen::Free, Seen::Held))
}

/// A write of the shared `candidate` (saved, or removed), by `write`: refused while another session
/// holds it exclusively (`holding`: this one does), and kept from it while it runs — the lock is
/// held shared meanwhile, so a session taking it waits for the write (`take` tries again) — and one
/// write at a time (beside it, `candidate.saving`), so what `write` checks it is still what it
/// writes over.
pub fn writing<T>(candidate: &Path, holding: bool, write: impl FnOnce() -> Result<T>) -> Result<T> {
    let _shared = if holding {
        None
    } else {
        let mut f = open(&path_for(candidate))?;
        if let Some(who) = holder_of(&mut f)? {
            bail!(
                "not done: the candidate is held exclusively by {} (`configure exclusive`): it can be read here, not changed, until that session leaves configuration mode",
                who.map_or("another session of yours".to_string(), |h| h.describe(&crate::history::Clock::system()))
            );
        }
        Some(f)
    };
    let one = open(&candidate.with_extension("saving"))?;
    one.lock().context("waiting for another session's write of the candidate")?;
    write()
}

/// Take the lock at `path` as `me`: the lock, and the stale holder it took over, if one was there.
/// Held by another session: refused, naming it.
pub fn take(path: &Path, me: &Holder, clock: &crate::history::Clock) -> Result<(Lock, Option<Holder>)> {
    use std::os::unix::fs::MetadataExt;
    let mut f = open(path)?;
    if !try_lock(&f, false)? {
        let who = written(&mut f)?.map_or("another session of yours".to_string(), |h| h.describe(clock));
        bail!("the candidate is held exclusively by {who}: it is yours to change again when that session leaves configuration mode (or ends)");
    }
    // The file locked is the one others will open: nothing put another in its place meanwhile.
    if f.metadata()?.ino() != std::fs::symlink_metadata(path)?.ino() {
        bail!("{} was replaced while it was being locked: not taken; `configure exclusive` again", path.display());
    }
    let stale = written(&mut f)?;
    f.set_len(0)?;
    f.rewind()?;
    f.write_all(me.render().as_bytes())?;
    f.sync_all()?;
    Ok((Lock { file: f }, stale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> crate::history::Clock {
        crate::history::Clock { now: 0, offset: 0 }
    }

    /// One holder at a time; others told who it is; let go when dropped; a lock whose session died
    /// (its process gone, so the kernel's lock gone with it) is found stale and taken over.
    #[test]
    fn one_holder_and_a_stale_one_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(&dir.path().join("candidate.json"));
        assert_eq!(path, dir.path().join("candidate.lock"));
        assert_eq!(seen(&path).unwrap(), Seen::Free, "no file: nobody");
        let me = Holder { pid: 4242, terminal: Some("pts/3".into()), since: "2026-11-07T14:02:51Z".into() };
        let (lock, stale) = take(&path, &me, &clock()).unwrap();
        assert_eq!(stale, None);
        assert_eq!(seen(&path).unwrap(), Seen::Held(Some(me.clone())));
        let e = format!("{:#}", take(&path, &Holder::me(), &clock()).unwrap_err());
        assert_eq!(e, "the candidate is held exclusively by torqcli process 4242 on pts/3, since 2026-11-07 14:02:51: it is yours to change again when that session leaves configuration mode (or ends)");
        drop(lock);
        assert_eq!(seen(&path).unwrap(), Seen::Free, "let go: no holder left in the file");

        // A session killed while holding it: its process is gone, and the kernel's lock with it; what
        // it wrote is still there.
        let gone = std::process::Command::new("true").spawn().unwrap();
        let pid = gone.id();
        let mut gone = gone;
        gone.wait().unwrap();
        let dead = Holder { pid, terminal: None, since: "2026-11-07T09:00:00Z".into() };
        std::fs::write(&path, dead.render()).unwrap();
        assert_eq!(seen(&path).unwrap(), Seen::Free, "found free: its holder ended, and the kernel let go with it");
        let (_lock, stale) = take(&path, &me, &clock()).unwrap();
        assert_eq!(stale, Some(dead), "taken over, and said whose it was");
        assert_eq!(seen(&path).unwrap(), Seen::Held(Some(me)));
    }

    /// Never through a link: the file is the person's own, in their private folder.
    #[test]
    fn never_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("candidate.lock");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &path).unwrap();
        assert!(take(&path, &Holder::me(), &clock()).is_err());
        assert!(!dir.path().join("elsewhere").exists());
    }
}
