//! What may be put back from a snapshot of the data (ADR-038), and how. The paths come from a person,
//! and the folders they name hold what any account put there.
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

/// Run as the owner: what root reads from the snapshot, on standard input, written to a name of its
/// own beside the target: a tar of a folder's contents, or a file's bytes. Private until it is done:
/// what comes back may be a secret. Links in the way lead nowhere the owner could not go.
const STAGE: &str = r#"set -eu
umask 077
s=$1 kind=$2 mode=$3 new=$4
mkdir -p -- "$(dirname -- "$s")"
if [ "$kind" = folder ]; then
  [ -z "$new" ] || mkdir -- "$s"
  tar -x -p --no-same-owner -f - -C "$s"
else
  dd of="$s" bs=1M conv=sparse status=none
  chmod -- "$mode" "$s"
fi
"#;

/// Run as the owner, once both sides are done: what is there aside, what was staged in its place.
const SWAP: &str = r#"set -eu
d=$1 aside=$2 s=$3
moved=
if [ -e "$d" ] || [ -L "$d" ]; then
  [ ! -e "$aside" ] && [ ! -L "$aside" ] || { echo "$aside is already there: move it first" >&2; exit 1; }
  mv -T -- "$d" "$aside"
  moved=1
fi
mv -T -- "$s" "$d" || { [ -z "$moved" ] || mv -T -- "$aside" "$d"; exit 1; }
"#;

/// Run as the owner: what was staged goes. Read-only folders come back read only, and `rm` needs in.
const CLEAN: &str = r#"chmod -R u+rwx -- "$1" 2>/dev/null
rm -rf -- "$1"
"#;

/// How much of what a command says is kept: a deep tree's errors run to megabytes, in root's memory.
const SAID: u64 = 4096;

/// One restore: what is read as root, and who writes it where. What is live is touched only once
/// all of it has been read and written, by a rename: a failure leaves it as it was.
pub struct Copy<'a> {
    pub source: PathBuf,
    pub to: &'a str,
    pub set_aside: &'a str,
    pub stage: &'a str,
    pub folder: bool,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// `to` sits in a folder of root's: root makes the stage, and does the swap.
    pub whole: bool,
}

impl Copy<'_> {
    pub fn run(&self) -> Result<()> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if fs::symlink_metadata(self.stage).is_ok() {
            bail!("{} is already there: move it first", self.stage);
        }
        if self.whole {
            if self.folder {
                fs::DirBuilder::new().mode(0o700).create(self.stage)?;
            } else {
                fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(self.stage)?;
            }
            std::os::unix::fs::chown(self.stage, Some(self.uid), Some(self.gid))?;
        }
        if let Err(e) = self.stream().and_then(|()| self.swap()) {
            self.clean();
            return Err(e);
        }
        Ok(())
    }

    /// Root's reader and the owner's writer, joined by a pipe and nothing else.
    fn stream(&self) -> Result<()> {
        let mut read = if self.folder {
            let mut c = crate::config::command("tar");
            c.args(["-c", "-S", "-f", "-", "--one-file-system", "--mode=u-s,g-s", "-C"]).arg(&self.source).arg(".");
            c
        } else {
            let mut c = crate::config::command("cat");
            c.arg("--").arg(&self.source);
            c
        };
        let mut reader = read.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().context("reading the snapshot")?;
        let pipe = reader.stdout.take().context("the snapshot's pipe")?;
        // Never set-id: what a person once could run as a group they have left, or as someone else.
        let mode = format!("{:o}", self.mode & 0o777);
        let wrote = self.as_owner(STAGE, &[self.stage, if self.folder { "folder" } else { "file" }, &mode, if self.whole { "" } else { "new" }], Stdio::from(pipe));
        let read_ok = reader.wait().context("reading the snapshot")?.success();
        wrote?;
        if !read_ok {
            bail!("the snapshot could not be read whole");
        }
        Ok(())
    }

    fn swap(&self) -> Result<()> {
        use rustix::fs::{renameat_with, RenameFlags, CWD};
        use std::os::unix::fs::MetadataExt;
        if !self.whole {
            return self.as_owner(SWAP, &[self.to, self.set_aside, self.stage], Stdio::null());
        }
        // As it stands now: a copy can take hours, and the checks before it are that old. Neither
        // rename replaces anything: what has appeared meanwhile is someone's.
        let there = match fs::symlink_metadata(self.to) {
            Ok(m) if m.is_symlink() || m.uid() != self.uid => bail!("{} has changed hands during the copy: not restored", self.to),
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        let keep = |from: &str, to: &str| renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE);
        if there {
            keep(self.to, self.set_aside).with_context(|| format!("setting {} aside", self.to))?;
        }
        keep(self.stage, self.to).map_err(anyhow::Error::from).with_context(|| format!("{} has appeared during the copy: not restored", self.to)).inspect_err(|_| {
            if there {
                let _ = keep(self.set_aside, self.to);
            }
        })
    }

    /// What was staged goes. In a folder of root's root removes it: `remove_dir_all` follows no link.
    fn clean(&self) {
        if !self.whole {
            let _ = self.as_owner(CLEAN, &[self.stage], Stdio::null());
        } else if self.folder {
            let _ = fs::remove_dir_all(self.stage);
        } else {
            let _ = fs::remove_file(self.stage);
        }
    }

    /// `script`, with no more than the owner's own rights. What it says is kept up to `SAID`.
    fn as_owner(&self, script: &str, args: &[&str], stdin: Stdio) -> Result<()> {
        let mut child = crate::config::command("setpriv")
            .args(["--reuid", &self.uid.to_string(), "--regid", &self.gid.to_string(), "--clear-groups", "--no-new-privs", "--", "sh", "-c", script, "sh"])
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running the copy")?;
        let mut said = Vec::new();
        if let Some(mut err) = child.stderr.take() {
            (&mut err).take(SAID).read_to_end(&mut said).context("reading what it said")?;
            std::io::copy(&mut err, &mut std::io::sink()).context("reading what it said")?;
        }
        if !child.wait().context("running the copy")?.success() {
            bail!("{}", String::from_utf8_lossy(&said).trim());
        }
        Ok(())
    }
}

/// A snapshot's label: digits, as `torq-snapshot` makes them. A ledger can come from elsewhere.
pub fn label_ok(label: &str) -> bool {
    !label.is_empty() && label.len() <= 20 && label.bytes().all(|b| b.is_ascii_digit())
}

/// The path as checked, whatever spelling it came in.
pub fn written_out(tree: &str, parts: &[String]) -> String {
    format!("{}/{}", if tree == "home" { "/home" } else { "/var/lib" }, parts.join("/"))
}

/// The primary group of the account with `uid` now, from /etc/passwd's text: a group kept from a
/// snapshot may be one the person has left.
pub fn primary_group(passwd: &str, uid: u32) -> Option<u32> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 4 && f[2].parse() == Ok(uid)).then(|| f[3].parse().ok()).flatten()
    })
}

/// Below `/var/lib`, what belongs to the machine and not to a service's data. `torq`, a subvolume of
/// its own, is not in the snapshots at all; the rest would put the machine's accounts back in time.
const NOT_RESTORED: &[&str] = &["torq", "nixos", "private", "systemd"];

/// Which tree of a snapshot `path` lies in (`home`, `var-lib`), and its parts below it. Only a clean
/// absolute path below a folder of `/home` or `/var/lib`, or that folder.
pub fn data_path(path: &str) -> Result<(&'static str, Vec<String>)> {
    if path.chars().any(char::is_control) || path.len() > 1024 {
        bail!("not a path to restore");
    }
    let mut parts = vec![];
    for c in Path::new(path).components().skip(1) {
        match c {
            Component::Normal(p) => parts.push(p.to_str().context("not a path to restore")?.to_string()),
            _ => bail!("{path:?}: a plain absolute path, with no `..`"),
        }
    }
    if !path.starts_with('/') {
        bail!("{path:?}: an absolute path, under /home or /var/lib");
    }
    let (tree, below) = match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["home", _, ..] => ("home", parts[1..].to_vec()),
        ["var", "lib", _, ..] => ("var-lib", parts[2..].to_vec()),
        _ => bail!("{path:?}: a snapshot holds what is below /home and /var/lib, one folder down at least"),
    };
    if tree == "var-lib" && NOT_RESTORED.contains(&below[0].as_str()) {
        bail!("{path:?}: the machine's own, not a service's data: not restored from a snapshot");
    }
    Ok((tree, below))
}

/// `parts`, below `root`, with no link on the way: the first part's metadata (whose it all is) and
/// the last's. A snapshot is read only, so what is checked here stays so.
pub fn no_links(root: &Path, parts: &[String]) -> Result<(fs::Metadata, fs::Metadata)> {
    let mut at = root.to_path_buf();
    let mut top = None;
    let mut last = None;
    for p in parts {
        at.push(p);
        let m = fs::symlink_metadata(&at).with_context(|| format!("{} is not in the snapshot", at.display()))?;
        if m.is_symlink() {
            bail!("{} is a link in the snapshot: name what it leads to", at.display());
        }
        top.get_or_insert(m.clone());
        last = Some(m);
    }
    Ok((top.context("nothing named")?, last.context("nothing named")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_may_be_restored() {
        assert_eq!(data_path("/home/alice").unwrap(), ("home", vec!["alice".to_string()]));
        assert_eq!(data_path("/home/alice/a b/c.txt").unwrap().1, ["alice", "a b", "c.txt"]);
        assert_eq!(data_path("/var/lib/postgresql/16").unwrap(), ("var-lib", vec!["postgresql".to_string(), "16".to_string()]));
        for bad in ["", "/", "/home", "/var", "/var/lib", "/etc/shadow", "/nix/store", "home/alice", "/home/alice/../bob", "/home/../etc", "/home/a\nb", "/var/lib/torq", "/var/lib/torq/state/ledger.jsonl", "/var/lib/nixos/uid-map", "/var/lib/private/x", "/var/lib/systemd/x", "/var/libx/y"] {
            assert!(data_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn labels_and_accounts() {
        assert!(label_ok("1793974624357"));
        for bad in ["", "/", "..", "1/2", "12a", "../../etc", "123456789012345678901"] {
            assert!(!label_ok(bad), "{bad:?}");
        }
        let passwd = "root:x:0:0::/root:/bin/sh\nbob:x:1001:100::/home/bob:/bin/sh\nodd:x:7\n";
        assert_eq!((primary_group(passwd, 1001), primary_group(passwd, 7), primary_group(passwd, 99)), (Some(100), None, None));
        assert_eq!(written_out("var-lib", &["pg".to_string(), "16".to_string()]), "/var/lib/pg/16");
    }

    #[test]
    fn a_link_in_a_snapshot_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("alice/docs")).unwrap();
        fs::write(d.path().join("alice/docs/a"), "x").unwrap();
        std::os::unix::fs::symlink("/etc", d.path().join("alice/etc")).unwrap();
        let parts = |s: &str| s.split('/').map(String::from).collect::<Vec<_>>();
        assert!(no_links(d.path(), &parts("alice/docs/a")).is_ok());
        assert!(no_links(d.path(), &parts("alice/etc/shadow")).is_err(), "through a link");
        assert!(no_links(d.path(), &parts("alice/etc")).is_err(), "a link itself");
        assert!(no_links(d.path(), &parts("alice/none")).is_err());
    }
}
