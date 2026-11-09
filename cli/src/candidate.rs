//! The candidate configuration (ADR-016, docs/CLI-DESIGN.md, "The commit model"): a person's
//! uncommitted `set` / `delete`, kept per user until `commit` or `rollback 0`, with the commit it was
//! started from (its base). The config repo itself is only ever read here — with git plumbing, never
//! its working tree, hooks or the caller's git settings — and only the broker writes it.

use crate::tree::{Config, Data, Schema};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The machine's config repo, read-only.
pub struct Repo {
    root: PathBuf,
}

/// One commit in the repo's history, newest first. (The configuration numbers are the ledger's, 4f:
/// git's history stands in for it only in tests.)
#[cfg(test)]
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub id: String,
    /// When it was made (ISO 8601, UTC).
    pub date: String,
    /// Its first line (the change's reason).
    pub subject: String,
}

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Repo { root: root.into() }
    }

    /// git on the repo, with nothing of the caller's environment or configuration that could point
    /// it elsewhere or run code: no GIT_* variables, no global or system config, no hooks, no
    /// fsmonitor. safe.directory names just this repo (it belongs to root).
    fn git(&self, args: &[&str]) -> Result<Vec<u8>> {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        let root = self.root.to_string_lossy();
        let out = c
            .env("TZ", "UTC") // dates in the ledger's time
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-C", &root, "-c", &format!("safe.directory={root}"), "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
            .args(args)
            .output()
            .context("running git")?;
        if !out.status.success() {
            bail!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(out.stdout)
    }

    pub fn head(&self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git(&["rev-parse", "--verify", "HEAD^{commit}"])?).trim().to_string())
    }

    /// The last `count` configurations, newest first (rollback 0 is the current one). The broker
    /// makes one commit per change on one branch, and resets an undone one away, so the branch's
    /// first-parent history is the machine's history of configurations.
    #[cfg(test)]
    pub fn history(&self, count: usize) -> Result<Vec<Commit>> {
        let out = self.git(&["log", "--first-parent", "-z", &format!("-n{count}"), "--date=iso-strict-local", "--format=%H%x1f%cd%x1f%s", "HEAD", "--"])?;
        String::from_utf8_lossy(&out)
            .split('\0')
            .filter(|r| !r.trim().is_empty())
            .map(|r| {
                let mut f = r.trim_start_matches('\n').splitn(3, '\x1f');
                Ok(Commit {
                    id: f.next().context("git log")?.to_string(),
                    date: f.next().context("git log")?.to_string(),
                    subject: f.next().unwrap_or_default().to_string(),
                })
            })
            .collect()
    }

    /// The whole configuration at commit `id`, written into `dest` (an empty folder): what the broker
    /// builds is the committed tree, not the repo's working copy.
    pub fn archive(&self, id: &str, dest: &Path) -> Result<()> {
        if !(id.len() == 40 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            bail!("not a commit id: {id:?}");
        }
        let tar = self.git(&["archive", "--format=tar", "--end-of-options", &format!("{id}^{{tree}}")])?;
        let mut child = Command::new("tar")
            .args(["-x", "--no-same-owner", "-C"])
            .arg(dest)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("running tar")?;
        child.stdin.take().context("tar's input")?.write_all(&tar)?;
        if !child.wait()?.success() {
            bail!("unpacking the configuration at {id} failed");
        }
        Ok(())
    }

    /// The files `paths` as reverting commit `after` (whose parent is `before`) leaves them on `head`:
    /// its own changes undone, later edits elsewhere in the same files kept — as `git revert` does it
    /// (its reverse patch, applied to `head`). None: it does not apply (a later change conflicts),
    /// so the broker would refuse the revert. Each file's content, or None if the revert deletes it.
    pub fn reverted_files(&self, head: &str, before: &str, after: &str, paths: &[&str]) -> Result<Option<Vec<(String, Option<String>)>>> {
        let mut args = vec!["diff", "--binary", "--no-color", "--no-ext-diff", before, after, "--"];
        args.extend(paths.iter().copied());
        let patch = self.git(&args)?;
        let scratch = tempfile::tempdir()?;
        let tree = scratch.path().join("tree");
        std::fs::create_dir(&tree)?;
        self.archive(head, &tree)?;
        let file = scratch.path().join("revert.patch");
        std::fs::write(&file, &patch)?;
        // A plain folder, not a repo: git apply only patches its files.
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        let st = c
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CEILING_DIRECTORIES", scratch.path())
            .current_dir(&tree)
            .args(["-c", "core.hooksPath=/dev/null", "apply", "-R", "--whitespace=nowarn"])
            .arg(&file)
            .output()
            .context("running git apply")?;
        if !st.status.success() {
            return Ok(None);
        }
        paths
            .iter()
            .map(|p| {
                let f = tree.join(p);
                Ok((p.to_string(), if f.exists() { Some(std::fs::read_to_string(&f).with_context(|| format!("{p}: not text"))?) } else { None }))
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }

    /// Whether commit `id` (40 hex digits) is in the repo.
    pub fn has_commit(&self, id: &str) -> Result<bool> {
        if !(id.len() == 40 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) {
            bail!("not a commit id: {id:?}");
        }
        // Through `git` (its environment cleared, this repo only): it fails when the commit is missing.
        Ok(self.git(&["cat-file", "-e", "--end-of-options", &format!("{id}^{{commit}}")]).is_ok())
    }

    /// The paths of every file of commit `rev`.
    pub fn paths(&self, rev: &str) -> Result<Vec<String>> {
        Ok(self.listing(rev)?.into_iter().map(|(_, _, _, p)| p).collect())
    }

    /// The commit before `id` (its first parent), if there is one.
    pub fn parent_of(&self, id: &str) -> Result<Option<String>> {
        if !self.has_commit(id)? {
            bail!("commit {id} is not in the config repo");
        }
        let out = self.git(&["rev-list", "--parents", "-n1", "--end-of-options", id]).unwrap_or_default();
        let ids: Vec<String> = String::from_utf8_lossy(&out).split_whitespace().map(String::from).collect();
        Ok(ids.get(1).cloned())
    }

    /// The entries of commit `rev`: (mode, kind, object id, path), every file.
    fn listing(&self, rev: &str) -> Result<Vec<(String, String, String, String)>> {
        // A commit id (from head() or history()) or HEAD — never anything git could read as an
        // option or a path (`--output=…`, `HEAD:system`).
        let is_id = rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if !(is_id || rev == "HEAD") {
            bail!("not a commit id: {rev:?}");
        }
        let commit = format!("{rev}^{{commit}}");
        let listing = self.git(&["ls-tree", "-r", "-z", "--full-tree", "--end-of-options", &commit])?;
        let mut out = Vec::new();
        for entry in listing.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let entry = String::from_utf8_lossy(entry);
            let (meta, path) = entry.split_once('\t').context("git ls-tree")?;
            let mut meta = meta.split(' ');
            let (mode, kind, id) = (meta.next().unwrap_or_default(), meta.next().unwrap_or_default(), meta.next().unwrap_or_default());
            out.push((mode.to_string(), kind.to_string(), id.to_string(), path.to_string()));
        }
        Ok(out)
    }

    /// The tree's files at `rev` (exact bytes): only paths the schema describes, and only regular
    /// files — a symlink or anything else at a tree path is refused, never followed.
    pub fn tree_files(&self, schema: &Schema, rev: &str) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut files = BTreeMap::new();
        for (mode, kind, id, path) in self.listing(rev)? {
            // (flake.nix: for its one setting, the software's source, software.rs.)
            if !schema.describes(&path) && crate::settings::top_of(&path).is_none() && path != crate::attach::FILE && path != "flake.nix" {
                continue;
            }
            // (A flake.nix that is not a plain file has no source: it is not refused here, only not read.)
            if path == "flake.nix" && (mode != "100644" || kind != "blob") {
                continue;
            }
            if mode != "100644" || kind != "blob" {
                bail!("{path} at {rev} is not a plain data file (git mode {mode}: a link, an executable or a folder)");
            }
            files.insert(path, self.git(&["cat-file", "blob", &id])?);
        }
        Ok(files)
    }

    /// One plain file of commit `rev` (None: not there), e.g. its `flake.lock`; a link or anything
    /// else there is refused, never followed (its "content" would be where it points).
    pub fn file_at(&self, rev: &str, path: &str) -> Result<Option<Vec<u8>>> {
        match self.listing(rev)?.into_iter().find(|(_, _, _, p)| p == path) {
            Some((mode, kind, id, _)) if mode == "100644" && kind == "blob" => Ok(Some(self.git(&["cat-file", "blob", &id])?)),
            Some((mode, ..)) => bail!("{path} at {rev} is not a plain file (git mode {mode})"),
            None => Ok(None),
        }
    }

    /// Every file of commit `rev` as committed — the tree's, the Nix files, `flake.lock` (a link:
    /// its target) — for a raw diff between two commits.
    pub fn all_files(&self, rev: &str) -> Result<BTreeMap<String, Vec<u8>>> {
        let mut files = BTreeMap::new();
        for (_, kind, id, path) in self.listing(rev)? {
            if kind == "blob" {
                files.insert(path, self.git(&["cat-file", "blob", &id])?);
            }
        }
        Ok(files)
    }
}

/// A person's candidate: the configuration they are editing, and the commit it started from.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub base: String,
    pub config: Config,
    /// The software (`flake.lock`) the candidate installs, if it changes it (`request system update`,
    /// `request system software rollback`); None: its base's.
    pub lock: Option<String>,
    /// The tree's NixOS settings (nixos/*.json, settings.rs).
    pub settings: crate::settings::Settings,
    /// The config files attached in the tree (config-files.json, attach.rs).
    pub attached: crate::attach::Attached,
    /// Config files as edited in the candidate (`file edit`): path → content (None: deleted).
    pub nix: BTreeMap<String, Option<String>>,
    /// The software's source, if the candidate changes it (`system software torq source`).
    pub source: Option<String>,
    /// The release of TorqOS whose signature was checked when software was loaded, with the version
    /// it holds (audit A3): `show | compare` names TorqOS by it while the lock is at that commit.
    pub release: Option<Release>,
    /// `commit check`'s result for these changes on this base, for `commit` to use (session.rs
    /// `Checked`): kept by its key, so any change makes it another key, and a commit evaluates again.
    pub checked: Option<crate::session::Checked>,
}

/// A release of TorqOS checked by its signature: its commit and the version it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub rev: String,
    pub version: crate::release::Version,
}

impl Candidate {
    /// A fresh candidate: the configuration at `rev` (`rollback <n>` passes an older one), based on
    /// the current HEAD — a rollback is a new change on top of today's configuration.
    pub fn from_repo(repo: &Repo, schema: &Schema, rev: &str) -> Result<Candidate> {
        let base = repo.head()?;
        // HEAD read once: a commit landing in between must not mix two configurations.
        let files = repo.tree_files(schema, if rev == "HEAD" { &base } else { rev })?;
        let config = schema.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        let settings = crate::settings::load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
        let attached = crate::attach::load(files.get(crate::attach::FILE).map(Vec::as_slice))?;
        Ok(Candidate { base, config, lock: None, settings, attached, nix: BTreeMap::new(), source: None, release: None, checked: None })
    }

    /// Where a user keeps their candidate: their own state directory, never shared.
    pub fn default_path() -> Result<PathBuf> {
        let state = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            Some(s) => PathBuf::from(s),
            None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state"),
        };
        Ok(state.join("torq/candidate.json"))
    }

    /// The saved candidate, if there is one (None: start from the active configuration). Only from
    /// a private folder, and never through a link.
    pub fn load(path: &Path, schema: &Schema) -> Result<Option<Candidate>> {
        if let Some(dir) = path.parent().filter(|d| d.exists()) {
            private_dir(dir)?;
        }
        if fs::symlink_metadata(path).is_ok_and(|m| !m.file_type().is_file()) {
            bail!("{} is not a plain file; remove it", path.display());
        }
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let v: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
        // 6: with the release its software was checked as; 5: with its software source; 4: with its config files; 3: with its NixOS settings; 2: with its software (`lock`); 1: neither — read as none. An older
        // torqcli refuses a newer one rather than silently drop what it does not know.
        if !(1..=6).contains(&v["version"].as_u64().unwrap_or(0)) {
            bail!("{}: not a candidate this torqcli reads; `rollback 0` starts a new one", path.display());
        }
        let base = v["base"].as_str().context("candidate without a base")?.to_string();
        let files = v["files"].as_object().context("candidate without files")?;
        let rendered: Vec<(String, String)> =
            files.iter().map(|(p, c)| Ok((p.clone(), c.as_str().context("candidate file")?.to_string()))).collect::<Result<_>>()?;
        let config = schema.load(rendered.iter().map(|(p, c)| (p.as_str(), c.as_bytes())))?;
        let lock = match &v["lock"] {
            serde_json::Value::Null => None,
            serde_json::Value::String(l) if serde_json::from_str::<serde_json::Value>(l).is_ok_and(|j| j.is_object()) => Some(l.clone()),
            _ => bail!("{}: its software (flake.lock) is not JSON; `rollback 0` starts a new candidate", path.display()),
        };
        let settings = match &v["settings"] {
            serde_json::Value::Null => crate::settings::Settings::new(),
            // Each a NixOS top-level name, as a file name of its own (`nixos/<name>.json`).
            serde_json::Value::Object(o) if o.iter().all(|(k, x)| x.is_object() && crate::settings::top_of(&format!("{}{k}.json", crate::settings::DIR)).is_some()) => {
                o.clone().into_iter().collect()
            }
            _ => bail!("{}: its settings are not what torqcli writes; `rollback 0` starts a new candidate", path.display()),
        };
        let attached = match &v["attached"] {
            serde_json::Value::Null => crate::attach::Attached::new(),
            a => crate::attach::load(Some(a.to_string().as_bytes())).with_context(|| path.display().to_string())?,
        };
        // Config files as edited here: each a .nix file of the repo, its text (or null: deleted).
        let nix = match &v["nix"] {
            serde_json::Value::Null => BTreeMap::new(),
            serde_json::Value::Object(o) => o
                .iter()
                .map(|(p, c)| {
                    crate::attach::valid(p)?;
                    match c {
                        serde_json::Value::Null => Ok((p.clone(), None)),
                        serde_json::Value::String(s) => Ok((p.clone(), Some(s.clone()))),
                        _ => bail!("{}: a config file's text is not text", path.display()),
                    }
                })
                .collect::<Result<_>>()?,
            _ => bail!("{}: its config files are not what torqcli writes; `rollback 0` starts a new candidate", path.display()),
        };
        let source = match &v["source"] {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => {
                crate::software::valid(s).with_context(|| path.display().to_string())?;
                Some(s.clone())
            }
            _ => bail!("{}: its software source is not what torqcli writes; `rollback 0` starts a new candidate", path.display()),
        };
        // (Kept only as torqcli writes it: a commit, and a version in the form shown.)
        // A check's result that does not read as torqcli writes it is no result: `commit` evaluates
        // again. (An older torqcli drops it, as unknown: the same.)
        let checked = serde_json::from_value(v["checked"].clone()).ok().flatten();
        let release = match &v["release"] {
            serde_json::Value::Null => None,
            serde_json::Value::Object(r) => match (r.get("rev").and_then(|x| x.as_str()), r.get("version").and_then(|x| x.as_str())) {
                (Some(rev), Some(number)) if rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()) && crate::release::is_version(number) => {
                    Some(Release { rev: rev.to_string(), version: crate::release::Version { number: number.to_string(), exact: r.get("exact").and_then(|x| x.as_bool()).unwrap_or(false) } })
                }
                _ => bail!("{}: its release is not what torqcli writes; `rollback 0` starts a new candidate", path.display()),
            },
            _ => bail!("{}: its release is not what torqcli writes; `rollback 0` starts a new candidate", path.display()),
        };
        Ok(Some(Candidate { base, config, lock, settings, attached, nix, source, release, checked }))
    }

    /// Save it atomically, readable by its owner only, in a folder only its owner can write: anyone
    /// else who could replace the file could change what the user commits under their name.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().context("candidate path")?;
        private_dir(dir)?;
        let files: serde_json::Map<String, serde_json::Value> =
            self.config.files.iter().map(|(p, d)| (p.clone(), d.render().into())).collect();
        let body = serde_json::to_vec_pretty(&serde_json::json!({ "version": 6, "base": self.base, "files": files, "lock": self.lock, "settings": self.settings, "attached": self.attached, "nix": self.nix, "source": self.source, "release": self.release.as_ref().map(|r| serde_json::json!({ "rev": r.rev, "version": r.version.number, "exact": r.version.exact })), "checked": self.checked }))?;
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?; // created 0600
        tmp.write_all(&body)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error).with_context(|| format!("saving {}", path.display()))?;
        fs::File::open(dir)?.sync_all()?;
        Ok(())
    }

    pub fn discard(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// What a saved candidate is, as one session last read or wrote it: its bytes' SHA-256 (None: there
/// is none). A session writes the shared candidate only if it is still what it read, so it never
/// overwrites what another session saved meanwhile.
pub type Print = Option<[u8; 32]>;

/// The saved candidate at `path` as it is now (never through a link: a link is not a candidate).
pub fn print_of(path: &Path) -> Result<Print> {
    use sha2::{Digest, Sha256};
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        Ok(m) if !m.file_type().is_file() => bail!("{} is not a plain file; remove it", path.display()),
        Ok(_) => Ok(Some(Sha256::digest(fs::read(path).with_context(|| format!("reading {}", path.display()))?).into())),
    }
}

/// Make sure `dir` exists, belongs to this user and only they can write it (created 0700).
pub fn private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    if !dir.exists() {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::DirBuilder::new().mode(0o700).create(dir).or_else(|e| if dir.is_dir() { Ok(()) } else { Err(e) })?;
    }
    let me = fs::metadata("/proc/self").context("reading /proc/self")?.uid();
    let m = fs::symlink_metadata(dir)?;
    if !m.is_dir() || m.uid() != me || m.permissions().mode() & 0o022 != 0 {
        bail!(
            "{} must be a folder of your own that only you can write (it holds your uncommitted configuration)",
            dir.display()
        );
    }
    Ok(())
}

/// The changes from the base's files (exact text) to the candidate's: per changed file, its path,
/// old text (None: new) and new text (None: deleted). A file whose data is unchanged is no change,
/// however it is formatted.
pub fn changes<'a>(schema: &Schema, base: &'a BTreeMap<String, Vec<u8>>, candidate: &Config) -> Result<Vec<(String, Option<&'a [u8]>, Option<String>)>> {
    let before = schema.load(base.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let paths: BTreeSet<&String> = base.keys().chain(candidate.files.keys()).collect();
    Ok(paths
        .into_iter()
        .filter(|p| before.files.get(*p) != candidate.files.get(*p))
        .map(|p| (p.clone(), base.get(p).map(Vec::as_slice), candidate.files.get(p).map(Data::render)))
        .collect())
}

/// The changes as one unified diff for `torq-broker propose` (git apply reads it): new files from
/// /dev/null, deleted ones to /dev/null.
/// One file's change: its path, its bytes in the base (None: absent), its new content (None: deleted).
pub type Change<'a> = (String, Option<&'a [u8]>, Option<String>);

/// The candidate's software as a change to its base's `flake.lock`, if it is one.
pub fn lock_change<'a>(base_lock: Option<&'a [u8]>, lock: &Option<String>) -> Option<Change<'a>> {
    let new = lock.as_ref()?;
    (base_lock != Some(new.as_bytes())).then(|| ("flake.lock".to_string(), base_lock, Some(new.clone())))
}

pub fn unified_diff(scratch: &Path, changes: &[(String, Option<&[u8]>, Option<String>)]) -> Result<String> {
    let bytes: Vec<RawChange> = changes.iter().map(|(p, o, n)| (p.clone(), *o, n.as_deref().map(str::as_bytes))).collect();
    unified_diff_bytes(scratch, &bytes)
}

/// One file's change as bytes: its path, before and after (None: absent).
pub type RawChange<'a> = (String, Option<&'a [u8]>, Option<&'a [u8]>);

/// As `unified_diff`, both sides as bytes (two commits' files, exactly as committed).
pub fn unified_diff_bytes(scratch: &Path, changes: &[RawChange]) -> Result<String> {
    let mut out = String::new();
    for (i, (path, old, new)) in changes.iter().enumerate() {
        let side = |name: &str, text: Option<&[u8]>| -> Result<(PathBuf, String)> {
            Ok(match text {
                Some(t) => {
                    let f = scratch.join(format!("{name}-{i}"));
                    fs::write(&f, t)?;
                    (f, format!("{name}/{path}"))
                }
                None => (PathBuf::from("/dev/null"), "/dev/null".to_string()),
            })
        };
        let (a, a_label) = side("a", *old)?;
        let (b, b_label) = side("b", *new)?;
        let o = Command::new("diff").arg("-u").args(["--label", &a_label, "--label", &b_label]).arg(&a).arg(&b).output().context("running diff")?;
        if o.status.code() != Some(1) {
            bail!("diff failed on {path}: {}", String::from_utf8_lossy(&o.stderr));
        }
        out.push_str(&String::from_utf8_lossy(&o.stdout));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }
    /// A revert's result is its reverse patch on HEAD: a later edit elsewhere in the same file is
    /// kept (as git revert keeps it), a new file goes, and a later change it collides with is None.
    #[test]
    fn a_revert_leaves_what_git_revert_leaves() {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t \
            && printf 'a\\nb\\nc\\nd\\ne\\nf\\ng\\n' > x.nix && git add -A && git commit -qm one \
            && sed -i s/^a$/A/ x.nix && printf 'new\\n' > y.nix && git add -A && git commit -qm two \
            && sed -i s/^g$/G/ x.nix && git commit -qam three");
        let r = Repo::new(d.path());
        let ids: Vec<String> = r.history(3).unwrap().into_iter().map(|c| c.id).collect();
        let (head, two, one) = (&ids[0], &ids[1], &ids[2]);
        let files = r.reverted_files(head, one, two, &["x.nix", "y.nix"]).unwrap().expect("applies");
        assert_eq!(files, [("x.nix".to_string(), Some("a\nb\nc\nd\ne\nf\nG\n".to_string())), ("y.nix".to_string(), None)]);
        // A later change to the same line: it does not apply, as git revert would conflict.
        sh(d.path(), "sed -i s/^A$/AA/ x.nix && git commit -qam four");
        let head = r.head().unwrap();
        assert_eq!(r.reverted_files(&head, one, two, &["x.nix"]).unwrap(), None);
    }

    /// A config repo with three configurations, as the broker would have made them.
    fn repo() -> (tempfile::TempDir, Repo) {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t \
            && mkdir -p system users/dev && echo '{ x = 1; }' > flake.nix \
            && printf '{ \"host-name\": \"one\" }\\n' > system/host.json && git add -A && git commit -q -m 'first' \
            && printf '[\\n  \"htop\"\\n]\\n' > system/packages.json && git add -A && git commit -q -m 'second' \
            && printf '{ \"host-name\": \"three\" }\\n' > system/host.json && git add -A && git commit -q -m 'third'");
        let r = Repo::new(d.path());
        (d, r)
    }

    #[test]
    fn reads_history_newest_first() {
        let (_d, r) = repo();
        let h = r.history(10).unwrap();
        assert_eq!(h.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["third", "second", "first"]);
        assert_eq!(h[0].id, r.head().unwrap());
    }

    #[test]
    fn reads_only_the_tree_files_at_a_commit() {
        let (_d, r) = repo();
        let s = schema();
        let now = r.tree_files(&s, "HEAD").unwrap();
        assert_eq!(now.keys().collect::<Vec<_>>(), ["flake.nix", "system/host.json", "system/packages.json"], "flake.nix too, for its one setting (software.rs); no other file");
        let first = r.tree_files(&s, &r.history(3).unwrap()[2].id).unwrap();
        assert_eq!(String::from_utf8_lossy(&first["system/host.json"]), "{ \"host-name\": \"one\" }\n");
    }

    #[test]
    fn a_link_at_a_tree_path_is_refused() {
        let (d, r) = repo();
        sh(d.path(), "ln -s ../flake.nix users/dev/packages.json && git add -A && git commit -q -m link");
        let e = format!("{:#}", r.tree_files(&schema(), "HEAD").unwrap_err());
        assert!(e.contains("users/dev/packages.json") && e.contains("not a plain data file"), "{e}");
    }

    /// `rollback 2` is the configuration two changes back, as a new change on today's HEAD.
    #[test]
    fn a_rollback_loads_an_old_configuration_on_the_current_base() {
        let (_d, r) = repo();
        let s = schema();
        let c = Candidate::from_repo(&r, &s, &r.history(3).unwrap()[2].id).unwrap();
        assert_eq!(c.base, r.head().unwrap());
        assert_eq!(s.show(&c.config, &[]).unwrap(), "system {\n    host-name one;\n}\n");
    }

    #[test]
    fn a_candidate_is_saved_privately_and_read_back() {
        let (_d, r) = repo();
        let s = schema();
        let mut c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        s.set(&mut c.config, &w("users dev packages jq")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torq/candidate.json");
        c.save(&path).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions()) & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(Candidate::load(&path, &s).unwrap(), Some(c.clone()));
        Candidate::discard(&path).unwrap();
        assert_eq!(Candidate::load(&path, &s).unwrap(), None);
        Candidate::discard(&path).unwrap(); // twice is fine
        fs::write(&path, r#"{"version":5,"base":"x","files":{},"source":"github:a/b\";x"}"#).unwrap();
        assert!(Candidate::load(&path, &s).is_err(), "a saved candidate's source is a plain one");
        fs::write(&path, r#"{"version":4,"base":"x","files":{},"nix":{"../etc/x.nix":"{}"}}"#).unwrap();
        assert!(Candidate::load(&path, &s).is_err(), "a saved candidate's config files are inside the repo");
        fs::write(&path, r#"{"version":7,"base":"x","files":{}}"#).unwrap();
        assert!(format!("{:#}", Candidate::load(&path, &s).unwrap_err()).contains("rollback 0"));
        // Its software is kept too; a saved one without (an older torqcli's) has none; one that is
        // not JSON is refused.
        c.lock = Some(r#"{"nodes":{}}"#.into());
        c.save(&path).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("\"version\": 6"), "an older torqcli refuses it, not drop its software");
        assert_eq!(Candidate::load(&path, &s).unwrap(), Some(c.clone()));
        fs::write(&path, format!(r#"{{"version":1,"base":"{}","files":{{}}}}"#, c.base)).unwrap();
        assert_eq!(Candidate::load(&path, &s).unwrap().unwrap().lock, None);
        fs::write(&path, format!(r#"{{"version":1,"base":"{}","files":{{}},"lock":"nope"}}"#, c.base)).unwrap();
        assert!(format!("{:#}", Candidate::load(&path, &s).unwrap_err()).contains("is not JSON"));
        // Its NixOS settings: kept, and each named as its file is.
        c.settings.insert("virtualisation".into(), serde_json::json!({"docker": {"enable": true}}));
        c.save(&path).unwrap();
        assert_eq!(Candidate::load(&path, &s).unwrap(), Some(c.clone()));
        fs::write(&path, format!(r#"{{"version":3,"base":"{}","files":{{}},"settings":{{"../users/alice/x":{{}}}}}}"#, c.base)).unwrap();
        assert!(format!("{:#}", Candidate::load(&path, &s).unwrap_err()).contains("not what torqcli writes"));
    }

    /// The diff for the broker: only files whose data changed, from their exact text, and it
    /// applies to the repo as `git apply` (the broker) reads it.
    #[test]
    fn the_diff_holds_exactly_the_changes_and_applies() {
        let (d, r) = repo();
        let s = schema();
        // Hand-formatted, but the same data: no change.
        sh(d.path(), "printf '{\"host-name\":\"three\"}' > system/host.json && git commit -qam reformat");
        let base = r.tree_files(&s, "HEAD").unwrap();
        let mut c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        assert!(changes(&s, &base, &c.config).unwrap().is_empty());
        s.set(&mut c.config, &w("system time-zone UTC")).unwrap(); // changes an existing file
        s.delete(&mut c.config, &w("system packages")).unwrap(); // deletes one
        s.set(&mut c.config, &w("users dev packages jq")).unwrap(); // adds one
        let ch = changes(&s, &base, &c.config).unwrap();
        assert_eq!(ch.iter().map(|(p, o, n)| (p.as_str(), o.is_some(), n.is_some())).collect::<Vec<_>>(),
            [("system/host.json", true, true), ("system/packages.json", true, false), ("users/dev/packages.json", false, true)]);
        let scratch = tempfile::tempdir().unwrap();
        let diff = unified_diff(scratch.path(), &ch).unwrap();
        fs::write(scratch.path().join("change.diff"), &diff).unwrap();
        sh(d.path(), &format!("git apply --index {}", scratch.path().join("change.diff").display()));
        let applied = fs::read_to_string(d.path().join("system/host.json")).unwrap();
        assert_eq!(applied, "{\n  \"host-name\": \"three\",\n  \"time-zone\": \"UTC\"\n}\n");
        assert!(!d.path().join("system/packages.json").exists());
        assert_eq!(fs::read_to_string(d.path().join("users/dev/packages.json")).unwrap(), "[\n  \"jq\"\n]\n");
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;

    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }

    /// A hand-ordered list with a repeat is the same configuration after a save and a load: no
    /// phantom change (review of d373172).
    #[test]
    fn list_order_is_no_change() {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t && mkdir system \
            && echo '[\"zsh\",\"htop\",\"zsh\"]' > system/packages.json && git add -A && git commit -qm x");
        let (r, s) = (Repo::new(d.path()), schema());
        let c = Candidate::from_repo(&r, &s, "HEAD").unwrap();
        let state = tempfile::tempdir().unwrap();
        let p = state.path().join("torq/candidate.json");
        c.save(&p).unwrap();
        let again = Candidate::load(&p, &s).unwrap().unwrap();
        assert!(changes(&s, &r.tree_files(&s, "HEAD").unwrap(), &again.config).unwrap().is_empty());
    }

    /// The release the software was checked as is kept with the candidate, and only as torqcli writes
    /// it: a version from elsewhere could otherwise be shown as the release's (audit A3).
    #[test]
    fn the_release_checked_is_kept() {
        let state = tempfile::tempdir().unwrap();
        let p = state.path().join("torq/candidate.json");
        let release = Release { rev: "0123456789abcdef0123456789abcdef01234567".into(), version: crate::release::Version { number: "0.3.0-rc1".into(), exact: true } };
        let c = Candidate { base: "x".into(), config: Config::default(), lock: None, settings: Default::default(), attached: Default::default(), nix: Default::default(), source: None, release: Some(release.clone()), checked: None };
        c.save(&p).unwrap();
        assert_eq!(Candidate::load(&p, &schema()).unwrap().unwrap().release, Some(release));
        let text = fs::read_to_string(&p).unwrap();
        for bad in [text.replace("\"0.3.0-rc1\"", "\"0.3.0\\u001b[2J\""), text.replace("0123456789abcdef0123", "--upload-pack=x")] {
            fs::write(&p, bad).unwrap();
            assert!(format!("{:#}", Candidate::load(&p, &schema()).unwrap_err()).contains("its release is not what torqcli writes"));
        }
    }

    #[test]
    fn the_candidate_folder_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("torq");
        let c = Candidate { base: "x".into(), config: Config::default(), lock: None, settings: Default::default(), attached: Default::default(), nix: Default::default(), source: None, release: None, checked: None };
        c.save(&dir.join("candidate.json")).unwrap();
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "created private");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        for e in [c.save(&dir.join("candidate.json")).unwrap_err(), Candidate::load(&dir.join("candidate.json"), &schema()).unwrap_err()] {
            assert!(format!("{e:#}").contains("only you can write"), "{e:#}");
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(dir.join("candidate.json")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", dir.join("candidate.json")).unwrap();
        assert!(format!("{:#}", Candidate::load(&dir.join("candidate.json"), &schema()).unwrap_err()).contains("not a plain file"));
    }

    #[test]
    fn only_commit_ids_reach_git() {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t && touch a && git add -A && git commit -qm x");
        let r = Repo::new(d.path());
        for bad in ["--output=/tmp/x", "HEAD:system", "HEAD~1", "main", "ABCDEF0123456789ABCDEF0123456789ABCDEF01", ""] {
            assert!(format!("{:#}", r.tree_files(&schema(), bad).unwrap_err()).contains("not a commit id"), "{bad}");
        }
        assert!(r.tree_files(&schema(), &r.head().unwrap()).is_ok());
    }
}

#[cfg(test)]
mod archive_tests {
    use super::*;

    /// `commit check` evaluates the committed base, as the broker builds it: not untracked files or
    /// edits in the repo's working copy (review of 4a12b45).
    #[test]
    fn archive_is_the_committed_tree() {
        let d = tempfile::tempdir().unwrap();
        let st = Command::new("sh").arg("-c").arg("git init -q -b main && git config user.name t && git config user.email t@t \
            && echo committed > a && git add a && git commit -qm x && echo edited > a && echo stray > untracked")
            .current_dir(d.path()).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success());
        let r = Repo::new(d.path());
        let out = tempfile::tempdir().unwrap();
        r.archive(&r.head().unwrap(), out.path()).unwrap();
        assert_eq!(fs::read_to_string(out.path().join("a")).unwrap(), "committed\n");
        assert!(!out.path().join("untracked").exists());
        assert!(r.archive("HEAD", out.path()).is_err(), "a commit id, nothing else");
    }
}
