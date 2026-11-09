//! What an evaluation of a committed configuration found, kept so it is not evaluated again: SSH as
//! the configuration at a commit has it (the state before a commit's change). A commit is its files
//! and its lock, so the same commit always evaluates the same. Kept in the person's own cache folder
//! (`$XDG_CACHE_HOME/torq`, else `~/.cache/torq`): made 0700, and used only while it is theirs and
//! no one else's at all (no group or other bits); each entry read and written through the folder
//! held open, never through a link. An entry that cannot be read is evaluated again, never trusted
//! half-read. Entries are named by what read them (`ssh_form`): another torqcli's are not this one's.

use crate::session::SshFacts;
use rustix::fs::{Mode, OFlags};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;

/// More than this is not an entry torqcli wrote.
const MAX: u64 = 1 << 20;

/// Where entries are kept; None: nowhere (no home, or a test that keeps none).
#[derive(Debug, Clone, Default)]
pub struct Cache(Option<PathBuf>);

impl Cache {
    /// None kept: everything is evaluated.
    pub fn none() -> Self {
        Cache(None)
    }

    /// In `dir` (a test's).
    #[cfg(test)]
    pub fn at(dir: &std::path::Path) -> Self {
        Cache(Some(dir.to_path_buf()))
    }

    /// The person's own: `$XDG_CACHE_HOME/torq` (an absolute one), else `~/.cache/torq`.
    pub fn personal() -> Self {
        let base = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
        Cache(base.map(|b| b.join("torq")))
    }

    /// The entry's name for `commit`: a commit id and nothing else names a file.
    fn name(commit: &str) -> Option<String> {
        let id = (40..=64).contains(&commit.len()) && commit.chars().all(|c| c.is_ascii_hexdigit());
        id.then(|| format!("{}-{commit}.json", crate::ssh_form()))
    }

    /// The folder, held open: made 0700 if it is not there (with `make`), refused unless it is a
    /// folder (not a link) of this user's that nobody else may even read.
    fn folder(&self, make: bool) -> Option<OwnedFd> {
        let dir = self.0.as_ref()?;
        if make && fs::symlink_metadata(dir).is_err() {
            use std::os::unix::fs::DirBuilderExt;
            fs::create_dir_all(dir.parent()?).ok()?;
            let _ = fs::DirBuilder::new().mode(0o700).create(dir);
        }
        let fd = rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()).ok()?;
        let st = rustix::fs::fstat(&fd).ok()?;
        let mine = st.st_uid == rustix::process::geteuid().as_raw();
        (mine && st.st_mode & 0o077 == 0).then_some(fd)
    }

    /// SSH as the configuration at `commit` has it, if it is kept and reads as torqcli wrote it.
    pub fn get(&self, commit: &str) -> Option<SshFacts> {
        let name = Self::name(commit)?;
        let dir = self.folder(false)?;
        let fd = rustix::fs::openat(&dir, name.as_str(), OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()).ok()?;
        let f = fs::File::from(fd);
        if !f.metadata().ok()?.is_file() {
            return None;
        }
        let mut text = Vec::new();
        f.take(MAX + 1).read_to_end(&mut text).ok()?;
        if text.len() as u64 > MAX {
            return None;
        }
        serde_json::from_slice(&text).ok()
    }

    /// Keep `facts` as the configuration at `commit`'s. Not kept is no failure: it is evaluated
    /// again next time.
    pub fn put(&self, commit: &str, facts: &SshFacts) {
        let (Some(name), Some(dir)) = (Self::name(commit), self.folder(true)) else { return };
        let Ok(body) = serde_json::to_vec(facts) else { return };
        // Written whole under a name of its own (created new: never an existing file or a link),
        // then renamed into place in the same folder: a rename replaces a link, never follows it.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let tmp = format!(".{name}.{}.{nanos}", std::process::id());
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let Ok(fd) = rustix::fs::openat(&dir, tmp.as_str(), flags, Mode::RUSR | Mode::WUSR) else { return };
        let written = fs::File::from(fd).write_all(&body).is_ok();
        if !written || rustix::fs::renameat(&dir, tmp.as_str(), &dir, name.as_str()).is_err() {
            let _ = rustix::fs::unlinkat(&dir, tmp.as_str(), rustix::fs::AtFlags::empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Ssh;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    fn facts(on: bool) -> SshFacts {
        SshFacts { state: Ssh { on, passwords: false }, admits: serde_json::json!({"owner": "dev"}) }
    }

    #[test]
    fn kept_by_commit_privately_and_never_through_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("cache/torq");
        let c = Cache::at(&dir);
        assert_eq!(c.get(C), None, "nothing kept yet");
        c.put(C, &facts(true));
        assert_eq!(c.get(C), Some(facts(true)));
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "made 0700");
        // Not a commit id: neither kept nor read.
        c.put("../x", &facts(true));
        assert_eq!(c.get("../x"), None);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "nothing else left behind");
        // What cannot be read is evaluated again: a broken entry, a link.
        let f = dir.join(Cache::name(C).unwrap());
        assert!(f.file_name().unwrap().to_string_lossy().starts_with(&crate::ssh_form()), "named by what read it");
        fs::write(&f, "{not json").unwrap();
        assert_eq!(c.get(C), None);
        let elsewhere = d.path().join("planted.json");
        fs::write(&elsewhere, serde_json::to_vec(&facts(false)).unwrap()).unwrap();
        fs::remove_file(&f).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &f).unwrap();
        assert_eq!(c.get(C), None, "a link is never followed");
        c.put(C, &facts(true));
        assert_eq!(fs::read(&elsewhere).unwrap(), serde_json::to_vec(&facts(false)).unwrap(), "nor written through");
        assert_eq!(c.get(C), Some(facts(true)));
        // A folder anyone else may read, or write, is not the person's own: not used.
        for mode in [0o755, 0o750, 0o777] {
            fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(c.get(C), None, "{mode:o}");
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(c.get(C), Some(facts(true)));
        // The folder itself a link: not used.
        let link = d.path().join("linked");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert_eq!(Cache::at(&link).get(C), None);
        assert_eq!(Cache::none().get(C), None);
    }
}
