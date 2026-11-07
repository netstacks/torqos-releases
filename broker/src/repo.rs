//! Git operations on the local config repo. Shells out to `git`; the broker identity is fixed.

/// Where the ledger lives on the remote: a branch, so that branch protection can keep it (ADR-036).
pub const REMOTE_LEDGER: &str = "refs/heads/torq-ledger";
/// How long one git call to the remote may take, at most, in seconds.
pub const NET_SECONDS: u64 = 60;
/// Where on the remote a configuration waits for this machine to take it (`request system pull`):
/// never `main`, which only the machine writes — a commit made here from it is not that commit, and
/// `main` is never forced.
pub const INCOMING: &str = "refs/heads/incoming";

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

/// A revision as git is given one here: a full commit id, `HEAD`, or a ref by its full name. Never
/// anything git could read as an option: a commit named in a ledger is text whoever wrote the ledger
/// chose (an adopted one, someone else's), and `--output=<file>` given to `git show` as root writes
/// that file (the review of 0.2.1, H1).
pub fn revision(r: &str) -> Result<&str> {
    let id = r.len() == 40 && r.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let named = r == "HEAD" || (r.starts_with("refs/") && r.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b)) && !r.contains(".."));
    if !(id || named) {
        bail!("{r:?} is not a commit: refused");
    }
    Ok(r)
}

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
        self.git_run(env, args, None)
    }

    /// git itself: no hooks, no fsmonitor, paths literal, the broker's identity; `limit`: killed
    /// after that many seconds (the network's calls).
    fn git_run(&self, env: &[(&str, &OsStr)], args: &[&str], limit: Option<u64>) -> Result<Vec<u8>> {
        let user_name = format!("user.name={NAME}");
        let user_email = format!("user.email={EMAIL}");
        let mut c = match limit {
            Some(s) => {
                let mut c = crate::config::command("timeout");
                c.args(["--kill-after=5", &s.to_string(), "git"]);
                c
            }
            None => crate::config::command("git"),
        };
        let out = c
            .envs(env.iter().copied())
            // A path is a path, never a pattern: `*?[` in a proposed file's name match only itself
            // (the audit of ADR-041: ls-files took the first file a pattern matched).
            .env("GIT_LITERAL_PATHSPECS", "1")
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
            // (124: timeout stopped it; 137: it had to kill it.)
            if limit.is_some() && matches!(out.status.code(), Some(124 | 137)) {
                let what = args.iter().find(|a| matches!(**a, "push" | "fetch" | "ls-remote")).unwrap_or(&"to the remote");
                bail!("git {what} took longer than {} s and was stopped", limit.unwrap_or(0));
            }
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(out.stdout)
    }

    /// git, to the remote (ADR-036): SSH and nothing else — no `ext::` (which runs a command), no
    /// `file://`, no other transport — with `ssh` as `ssh_command` names it (its key, its pinned
    /// host key), objects checked as they come, no submodule followed, and stopped after
    /// `NET_SECONDS` whatever the remote does (a remote that answers keepalives but sends nothing
    /// must not hold the broker's lock: the review of ADR-036).
    fn git_net(&self, ssh_command: &str, args: &[&str]) -> Result<String> {
        let mut a = vec![
            "-c", "protocol.allow=never", "-c", "protocol.ssh.allow=always", "-c", "credential.helper=", "-c", "core.askPass=",
            "-c", "fetch.fsckObjects=true", "-c", "transfer.fsckObjects=true", "-c", "submodule.recurse=false",
        ];
        a.extend_from_slice(args);
        let env = [("GIT_SSH_COMMAND", OsStr::new(ssh_command)), ("GIT_TERMINAL_PROMPT", OsStr::new("0"))];
        Ok(String::from_utf8_lossy(&self.git_run(&env, &a, Some(NET_SECONDS))?).into_owned())
    }

    /// Record the ledger as `refs/torq/ledger`: a commit holding `ledger.jsonl` as it is now, on top
    /// of `parent` — the remote's newest, when it has one, so that the chain is always the remote's
    /// and more (a local ref moved or lost cannot fork it: the review of ADR-036). Nothing new when
    /// the ledger is what `parent` holds.
    pub fn record_ledger(&self, ledger: &str, texts: &[(String, String)], entries: usize, parent: Option<&str>) -> Result<String> {
        let blob = self.git_stdin(&["hash-object", "-w", "--stdin"], ledger)?.trim().to_string();
        let mut listing = format!("100644 blob {blob}\tledger.jsonl\n");
        // Proposals' diffs and effects, by hash (N19): the ledger names them, the remote keeps them too.
        if !texts.is_empty() {
            let mut sub = String::new();
            for (sha, text) in texts {
                let b = self.git_stdin(&["hash-object", "-w", "--stdin"], text)?.trim().to_string();
                sub.push_str(&format!("100644 blob {b}\t{sha}\n"));
            }
            let t = self.git_stdin(&["mktree"], &sub)?.trim().to_string();
            listing.push_str(&format!("040000 tree {t}\tproposals\n"));
        }
        let tree = self.git_stdin(&["mktree"], &listing)?.trim().to_string();
        if let Some(p) = parent {
            if self.git(&["rev-parse", &format!("{p}^{{tree}}")])?.trim() == tree {
                self.git(&["update-ref", "refs/torq/ledger", p])?;
                return Ok(p.to_string());
            }
        }
        let msg = format!("ledger: {entries} entries");
        let mut args = vec!["commit-tree", tree.as_str(), "-m", msg.as_str()];
        if let Some(p) = parent {
            args.extend(["-p", p]);
        }
        let commit = self.git(&args)?.trim().to_string();
        self.git(&["update-ref", "refs/torq/ledger", &commit])?;
        Ok(commit)
    }
    // (The ledger, on the remote, is a branch: `torq-ledger`. Branch protection covers branches, so
    // the remote can refuse to rewrite or delete it — GitHub's rulesets, for one, cover nothing else.)

    /// Push the ledger's ref to `url` — never forced: the remote's history only grows.
    pub fn push_ledger(&self, url: &str, ssh_command: &str) -> Result<()> {
        let ledger = format!("refs/torq/ledger:{REMOTE_LEDGER}");
        self.git_net(ssh_command, &["push", "--porcelain", "--no-recurse-submodules", "--", url, &ledger])?;
        Ok(())
    }

    /// Push the configuration (HEAD) as the remote's `main` — never forced. Apart from the ledger: a
    /// `main` the remote refuses (another history there) must not keep the ledger from it (the second
    /// review of ADR-036).
    pub fn push_main(&self, url: &str, ssh_command: &str, rev: &str) -> Result<()> {
        revision(rev)?;
        self.git_net(ssh_command, &["push", "--porcelain", "--no-recurse-submodules", "--", url, &format!("{rev}:refs/heads/main")])?;
        Ok(())
    }

    /// The remote's ledger: its newest commit, fetched as `refs/torq/remote-ledger` (None: it has
    /// none).
    pub fn fetch_remote_ledger(&self, url: &str, ssh_command: &str) -> Result<Option<String>> {
        let refs = self.git_net(ssh_command, &["ls-remote", "--", url, REMOTE_LEDGER])?;
        if refs.trim().is_empty() {
            return Ok(None);
        }
        self.git_net(ssh_command, &["fetch", "--no-tags", "--quiet", "--no-recurse-submodules", "--no-auto-gc", "--no-auto-maintenance", "--", url, &format!("+{REMOTE_LEDGER}:refs/torq/remote-ledger")])?;
        Ok(Some(self.git(&["rev-parse", "refs/torq/remote-ledger^{commit}"])?.trim().to_string()))
    }

    /// The remote's `incoming` branch — what a person (or another machine, a pull request) put there
    /// for this machine to take — fetched as `refs/torq/incoming`: its commit (None: there is none).
    /// Only fetched: what is in it reaches the machine as a person's commit, or not at all.
    pub fn fetch_incoming(&self, url: &str, ssh_command: &str) -> Result<Option<String>> {
        let refs = self.git_net(ssh_command, &["ls-remote", "--", url, INCOMING])?;
        if refs.trim().is_empty() {
            return Ok(None);
        }
        self.git_net(ssh_command, &["fetch", "--no-tags", "--quiet", "--no-recurse-submodules", "--no-auto-gc", "--no-auto-maintenance", "--", url, &format!("+{INCOMING}:refs/torq/incoming")])?;
        Ok(Some(self.git(&["rev-parse", "refs/torq/incoming^{commit}"])?.trim().to_string()))
    }

    /// A staging repository for adopting (ADR-036): bare, holding the remote's `main` and ledger.
    pub fn stage_from(&self, url: &str, ssh_command: &str) -> Result<()> {
        let _ = std::fs::remove_dir_all(&self.root);
        let ok = crate::config::command("git").args(["init", "-q", "--bare"]).arg(&self.root).status().context("running git init")?.success();
        if !ok {
            bail!("git init failed for {}", self.root.display());
        }
        self.git_net(ssh_command, &["fetch", "--no-tags", "--quiet", "--no-recurse-submodules", "--no-auto-gc", "--no-auto-maintenance", "--", url, "+refs/heads/main:refs/heads/main", &format!("+{REMOTE_LEDGER}:refs/torq/remote-ledger")])?;
        Ok(())
    }

    /// The size of a file of commit `rev` (None: it has none).
    pub fn size_at(&self, rev: &str, path: &str) -> Option<u64> {
        revision(rev).ok()?;
        self.git(&["cat-file", "-s", &format!("{rev}:{path}")]).ok()?.trim().parse().ok()
    }

    /// A file of commit `rev` as text, if it is at most `max` bytes (None: none, or larger).
    pub fn text_at_most(&self, rev: &str, path: &str, max: u64) -> Option<String> {
        revision(rev).ok()?;
        let object = format!("{rev}:{path}");
        let size: u64 = self.git(&["cat-file", "-s", &object]).ok()?.trim().parse().ok()?;
        if size > max {
            return None;
        }
        self.git(&["cat-file", "blob", &object]).ok()
    }

    /// The ledger of a ledger commit, refused above `max` bytes.
    pub fn ledger_at_most(&self, commit: &str, max: u64) -> Result<String> {
        revision(commit)?;
        self.text_at_most(commit, "ledger.jsonl", max).with_context(|| format!("the ledger at {commit} is missing, or larger than {max} bytes"))
    }

    /// Each commit of the machine's history, with who its message says approved it and which proposal it
    /// carries: what the ledger is checked against (`Broker::commits_disagree`).
    pub fn commit_trailers(&self) -> Result<std::collections::HashMap<String, (String, String)>> {
        let format = "--format=%H%x09%(trailers:key=Approved-By,valueonly,separator=)%x09%(trailers:key=Proposal,valueonly,separator=)";
        let out = self.git(&["log", "HEAD", format])?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let mut f = l.split('\t');
                Some((f.next()?.to_string(), (f.next()?.to_string(), f.next()?.to_string())))
            })
            .collect())
    }

    /// Every path of commit `rev`'s tree, with its mode (100644, 100755, 120000 a link, 160000 …).
    pub fn tree_paths(&self, rev: &str) -> Result<Vec<(String, String)>> {
        revision(rev)?;
        let out = self.git(&["ls-tree", "-r", "-z", rev])?;
        Ok(out.split('\0').filter(|l| !l.is_empty()).filter_map(|l| {
            let (meta, path) = l.split_once('\t')?;
            Some((meta.split(' ').next()?.to_string(), path.to_string()))
        }).collect())
    }


    /// The commit `name` points to.
    pub fn commit_of(&self, name: &str) -> Result<String> {
        Ok(self.git(&["rev-parse", &format!("{name}^{{commit}}")])?.trim().to_string())
    }

    /// Become a working copy of the staging repository at `stage` (`main`, its history), this
    /// directory holding at most what is about to be replaced; the ledger's ref taken too, and no
    /// remote left behind (the broker names its remote itself).
    pub fn clone_stage(&self, stage: &Path) -> Result<()> {
        let _ = std::fs::remove_dir_all(&self.root);
        let ok = crate::config::command("git").args(["clone", "-q", "--no-hardlinks", "--branch", "main", "--"]).arg(stage).arg(&self.root).status().context("running git clone")?.success();
        if !ok {
            bail!("git clone of the staged repository failed");
        }
        let stage = path::absolute(stage)?.to_string_lossy().into_owned();
        self.git(&["fetch", "-q", "--", &stage, "+refs/torq/remote-ledger:refs/torq/remote-ledger"])?;
        self.git(&["remote", "remove", "origin"])?;
        Ok(())
    }

    /// Commit `files`, whatever the tree's ignore rules say, with `message`. The new HEAD.
    pub fn commit_files(&self, files: &[&str], message: &str) -> Result<String> {
        let mut add = vec!["add", "-f", "--"];
        add.extend_from_slice(files);
        self.git(&add)?;
        self.git(&["commit", "-q", "--allow-empty", "-m", message])?;
        self.sync()?;
        self.head()
    }

    /// Whether the remote's ledger history is one line — no merge anywhere in it: a merge would let a
    /// rewrite sit beside the history it rewrites (the review of ADR-036, H1).
    pub fn ledger_history_linear(&self, tip: &str) -> Result<bool> {
        revision(tip)?;
        Ok(self.git(&["rev-list", "--count", "--min-parents=2", tip])?.trim() == "0")
    }

    /// Whether `ancestor` is in `tip`'s history.
    pub fn has_ancestor(&self, ancestor: &str, tip: &str) -> Result<bool> {
        revision(ancestor)?;
        revision(tip)?;
        self.is_ancestor(ancestor, tip)
    }

    /// The ledger commits after `since` (exclusive; None: from the first), oldest first, up to `tip`.
    pub fn ledger_commits(&self, since: Option<&str>, tip: &str) -> Result<Vec<String>> {
        revision(tip)?;
        if let Some(s) = since {
            revision(s)?;
        }
        let range = since.map_or(tip.to_string(), |s| format!("{s}..{tip}"));
        Ok(self.git(&["rev-list", "--reverse", &range])?.lines().map(String::from).collect())
    }

    /// The ledger a ledger commit holds.
    /// The proposals' texts a ledger commit keeps (N19): each name, and its size.
    pub fn ledger_text_sizes(&self, commit: &str) -> Result<Vec<(String, u64)>> {
        revision(commit)?;
        let out = self.git(&["ls-tree", "-l", "-z", commit, "proposals/"])?;
        Ok(out
            .split('\0')
            .filter(|l| !l.is_empty())
            .filter_map(|l| {
                let (meta, path) = l.split_once('\t')?;
                let size = meta.split_whitespace().nth(3)?.parse().ok()?;
                Some((path.strip_prefix("proposals/")?.to_string(), size))
            })
            .collect())
    }

    /// One of them.
    pub fn ledger_text(&self, commit: &str, name: &str) -> Result<String> {
        revision(commit)?;
        self.git(&["cat-file", "blob", &format!("{commit}:proposals/{name}")])
    }

    pub fn ledger_at(&self, commit: &str) -> Result<String> {
        revision(commit)?;
        self.git(&["cat-file", "blob", &format!("{commit}:ledger.jsonl")])
    }

    fn git_stdin(&self, args: &[&str], input: &str) -> Result<String> {
        use std::io::Write as _;
        let mut child = crate::config::command("git")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .arg("-C")
            .arg(&self.root)
            .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("running git")?;
        child.stdin.take().context("git's input")?.write_all(input.as_bytes())?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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

    /// What the diff in `diff_path` does to HEAD, as git itself writes it: the change a person is
    /// shown (the audit of ADR-041: a diff's own text can say one thing while `git apply` does
    /// another — lines past a hunk's count, text before a header, one file patched twice).
    pub fn proposal_effect(&self, diff_path: &Path) -> Result<String> {
        // --text: a .gitattributes (`-diff`, `binary`) cannot turn the change into "Binary files differ".
        self.with_proposal_index(diff_path, |env| self.git_env(env, &["diff", "--cached", "--no-renames", "--no-color", "--no-ext-diff", "--no-textconv", "--text", "HEAD"]))
    }

    /// A file as HEAD has it: None when HEAD has none at `path`; refused when it is not a regular
    /// file or is larger than `max_bytes`.
    pub fn file_at_head(&self, path: &str, max_bytes: u64) -> Result<Option<Vec<u8>>> {
        // "<mode> <type> <object>\t<path>\0", or nothing when there is none.
        let entry = self.git(&["ls-tree", "-z", "HEAD", "--", path])?;
        let Some((meta, _)) = entry.split_once('\t') else {
            return Ok(None);
        };
        let mut fields = meta.split(' ');
        let (Some(mode), Some(_), Some(object)) = (fields.next(), fields.next(), fields.next()) else {
            bail!("unexpected git ls-tree output for {path}: {entry:?}");
        };
        if mode != "100644" {
            bail!("{path} is not a regular file in the configuration");
        }
        let size: u64 = self.git(&["cat-file", "-s", object])?.trim().parse().context("blob size")?;
        if size > max_bytes {
            bail!("{path} is larger than {max_bytes} bytes");
        }
        Ok(Some(self.git_env_bytes(&[], &["cat-file", "blob", object])?))
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
        revision(commit)?;
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
        revision(commit)?;
        self.git(&["reset", "-q", "--hard", commit])?;
        self.sync()?;
        Ok(())
    }

    /// Keep a commit reachable after a reset: the reflog alone expires, and `git gc` would drop it.
    pub fn keep(&self, commit: &str) -> Result<()> {
        revision(commit)?;
        self.git(&["update-ref", &format!("refs/torq/discarded/{commit}"), commit])?;
        self.sync()
    }

    pub fn is_ancestor(&self, ancestor: &str, of: &str) -> Result<bool> {
        revision(ancestor)?;
        revision(of)?;
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
        revision(commit)?;
        self.git(&["show", "--stat", "--patch", commit])
    }
}
