//! Git operations on the local config repo. Shells out to `git`; the broker identity is fixed.

use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{self, Path, PathBuf};


pub struct Repo {
    root: PathBuf,
}

/// A file as a proposal would leave it (`Repo::proposed_files`).
#[derive(Debug)]
pub struct ProposedFile {
    /// The git mode: "100644" for a regular file; "100755", "120000" (symlink), "160000" otherwise.
    pub mode: String,
    pub size: u64,
    /// The exact bytes; empty when the file is larger than was asked for.
    pub content: Vec<u8>,
}

const NAME: &str = "torq-broker";
const EMAIL: &str = "broker@torq.local";

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        self.git_env(&[], args)
    }

    fn git_env(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git_env_bytes(env, args)?).to_string())
    }

    /// Like `git_env`, but stdout's exact bytes (file contents must not be "repaired" into text).
    fn git_env_bytes(&self, env: &[(&str, &OsStr)], args: &[&str]) -> Result<Vec<u8>> {
        let user_name = format!("user.name={NAME}");
        let user_email = format!("user.email={EMAIL}");
        let out = crate::config::command("git")
            .envs(env.iter().copied())
            .arg("-C")
            .arg(&self.root)
            .arg("-c")
            .arg(&user_name)
            .arg("-c")
            .arg(&user_email)
            .arg("-c")
            .arg("commit.gpgsign=false") // TODO(M3): sign with the broker key
            // The ledger is fsynced on every append; git must not fall behind it on power loss.
            .arg("-c")
            .arg("core.fsync=all")
            // Its advice is about resolving conflicts by hand, in a repo only the broker may write.
            .arg("-c")
            .arg("advice.mergeConflict=false")
            // Nothing in the repo runs as root: not its hooks (`.git/hooks`), not an fsmonitor it names
            // (as torqcli reads it; audit of 2026-10-31).
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// Flush the repo's whole filesystem (`syncfs`): working tree, objects, refs and directory
    /// renames. `core.fsync` covers only git's own files, but the rebuild builds the working tree
    /// and the ledger is fsynced on every append, so after a power loss the repo must not be behind
    /// either. Called after every write, before the rebuild and the ledger append.
    fn sync(&self) -> Result<()> {
        let st = crate::config::command("sync").arg("--file-system").arg(&self.root).status().context("running sync")?;
        if !st.success() {
            bail!("sync --file-system {} failed ({st})", self.root.display());
        }
        Ok(())
    }

    pub fn is_initialised(&self) -> bool {
        self.root.join(".git").exists()
    }

    /// Initialise the repo (if needed) and commit whatever is present as the baseline.
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        if !self.is_initialised() {
            self.git(&["init", "-q", "-b", "main"])?;
        }
        self.git(&["add", "-A"])?;
        if self.git(&["status", "--porcelain"])?.trim().is_empty() && self.head().is_ok() {
            return Ok(());
        }
        self.git(&["commit", "-q", "--allow-empty", "-m", "Baseline configuration\n\nLedger-Entry: init"])?;
        self.sync()?;
        Ok(())
    }

    /// How many commits HEAD's history has.
    pub fn commit_count(&self) -> Result<usize> {
        self.git(&["rev-list", "--count", "HEAD"])?.trim().parse().context("reading git's commit count")
    }

    pub fn head(&self) -> Result<String> {
        Ok(self.git(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    pub fn ensure_clean(&self) -> Result<()> {
        let s = self.git(&["status", "--porcelain"])?;
        if !s.trim().is_empty() {
            bail!("config repo has uncommitted changes; only the broker may write to it:\n{s}");
        }
        Ok(())
    }

    /// Every path a unified diff would add, modify or delete, without applying it.
    ///
    /// The diff is applied to a scratch index built from HEAD and compared with HEAD, so this is
    /// what the diff *does*, not what its headers say: a rename reports both the old and the new
    /// path (numstat reports only the new one), and names are raw (`-z`), not C-quoted.
    pub fn diff_files(&self, diff_path: &Path) -> Result<Vec<String>> {
        self.with_proposal_index(diff_path, |env| {
            let out = self.git_env(env, &["diff", "--cached", "--name-only", "--no-renames", "-z", "HEAD"])?;
            Ok(out.split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
        })
    }

    /// Each of `paths` as the diff would leave it: its git mode, size and — when at most
    /// `max_bytes` — exact content; None when the diff deletes it. Read from the same scratch index
    /// as `diff_files`, so it is what the proposal *does*, never the repo's own tree.
    pub fn proposed_files(&self, diff_path: &Path, paths: &[String], max_bytes: u64) -> Result<Vec<(String, Option<ProposedFile>)>> {
        self.with_proposal_index(diff_path, |env| {
            let mut out = Vec::new();
            for path in paths {
                // "<mode> <object> <stage>\t<path>\0", or nothing when the path is gone.
                let entry = self.git_env(env, &["ls-files", "--stage", "-z", "--", path])?;
                let Some((meta, _)) = entry.split_once('\t') else {
                    out.push((path.clone(), None));
                    continue;
                };
                let mut fields = meta.split(' ');
                let (Some(mode), Some(object)) = (fields.next(), fields.next()) else {
                    bail!("unexpected git ls-files output for {path}: {entry:?}");
                };
                let size: u64 = self.git_env(env, &["cat-file", "-s", object])?.trim().parse().context("blob size")?;
                let content = if size <= max_bytes { self.git_env_bytes(env, &["cat-file", "blob", object])? } else { Vec::new() };
                out.push((path.clone(), Some(ProposedFile { mode: mode.to_string(), size, content })));
            }
            Ok(out)
        })
    }

    /// Run `f` with GIT_INDEX_FILE pointing at a scratch index holding HEAD with the diff applied:
    /// the proposal's result, without touching the repo's own index or working tree.
    fn with_proposal_index<T>(&self, diff_path: &Path, f: impl FnOnce(&[(&str, &OsStr)]) -> Result<T>) -> Result<T> {
        let p = path::absolute(diff_path)?.to_string_lossy().into_owned();
        let index = path::absolute(self.root.join(".git/torq-proposal.index"))?;
        let env = [("GIT_INDEX_FILE", index.as_os_str())];
        let result = (|| {
            self.git_env(&env, &["read-tree", "HEAD"])?;
            self.git_env(&env, &["apply", "--cached", p.as_str()])
                .context("diff does not apply cleanly to the current config")?;
            f(&env)
        })();
        let _ = std::fs::remove_file(&index);
        result
    }

    pub fn apply_and_commit(&self, diff_path: &Path, message: &str) -> Result<String> {
        self.ensure_clean()?;
        let p = path::absolute(diff_path)?.to_string_lossy().into_owned();
        self.git(&["apply", "--index", p.as_str()])?;
        self.git(&["commit", "-q", "-m", message])?;
        self.sync()?;
        self.head()
    }

    pub fn revert_and_commit(&self, commit: &str, message: &str) -> Result<String> {
        self.ensure_clean()?;
        if let Err(e) = self.git(&["revert", "--no-commit", commit]) {
            // A conflicting revert leaves REVERT_HEAD and conflict markers behind, which would
            // block every later broker operation. Put the repo back exactly as it was.
            let _ = self.git(&["revert", "--abort"]);
            self.git(&["reset", "-q", "--hard", "HEAD"])?;
            self.sync()?;
            return Err(e.context(format!("cannot undo {commit} cleanly: later changes conflict with it \
                 (undo those first, newest first, or propose the opposite change)")));
        }
        self.git(&["commit", "-q", "-m", message])?;
        self.sync()?;
        self.head()
    }

    pub fn hard_reset(&self, commit: &str) -> Result<()> {
        self.git(&["reset", "-q", "--hard", commit])?;
        self.sync()?;
        Ok(())
    }

    /// Keep a commit reachable after a reset: the reflog alone expires, and `git gc` would drop it.
    pub fn keep(&self, commit: &str) -> Result<()> {
        self.git(&["update-ref", &format!("refs/torq/discarded/{commit}"), commit])?;
        self.sync()
    }

    pub fn is_ancestor(&self, ancestor: &str, of: &str) -> Result<bool> {
        let st = crate::config::command("git")
            .arg("-C")
            .arg(&self.root)
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(["merge-base", "--is-ancestor", ancestor, of])
            .status()
            .context("running git")?;
        match st.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => bail!("git merge-base --is-ancestor {ancestor} {of} failed ({st})"),
        }
    }

    pub fn index_lock(&self) -> PathBuf {
        self.root.join(".git/index.lock")
    }

    /// Remove an `index.lock` left by a git process that died. The caller must first check that no
    /// process still has it open: git children do not inherit the broker lock, so a git started by
    /// a broker that was just killed may still be finishing.
    pub fn remove_stale_index_lock(&self) -> Result<()> {
        match std::fs::remove_file(self.index_lock()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// A commit capturing uncommitted changes (tracked and untracked), without touching the tree or
    /// any ref; None when the tree is clean. Used to keep what a reset is about to destroy (it stages
    /// everything to include untracked files; the reset then rewrites the index anyway).
    pub fn snapshot_dirty(&self) -> Result<Option<String>> {
        if self.git(&["status", "--porcelain"])?.trim().is_empty() {
            return Ok(None);
        }
        self.git(&["add", "-A"])?;
        let stash = self.git(&["stash", "create", "uncommitted changes discarded by reconcile"])?;
        Ok(Some(stash.trim().to_string()).filter(|s| !s.is_empty()))
    }

    pub fn show(&self, commit: &str) -> Result<String> {
        self.git(&["show", "--stat", "--patch", commit])
    }
}
