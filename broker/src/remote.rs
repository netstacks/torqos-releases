//! The config repo's remote (ADR-036): where the machine's configuration and its ledger also live,
//! off the machine.
//!
//! After every change the broker pushes the configuration (`main`) and the ledger — as
//! the branch `torq-ledger` (here `refs/torq/ledger`), a chain of commits each holding the ledger as it was — never forced: with the
//! remote refusing to rewrite (branch protection), what was pushed stays. The ledger here must then
//! always be the remote's and more: anything else is a rewritten local history (`verify`, `status`).
//! Pulling is a proposal, never an apply (`request system pull`, later).
//!
//! Over SSH only, with a key of the machine's own (a deploy key, one repository), in the broker's
//! state directory: root's, never the agent's. The remote's host key is pinned on first contact.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Whether `url` is a remote the broker pushes to: SSH, as `ssh://[user@]host[:port]/path` or
/// `user@host:path`. Nothing else — a git URL can name a command to run (`ext::`), a local path, or
/// a transport git would ask a helper for; as root, none of them.
pub fn url_ok(url: &str) -> bool {
    let safe = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-/~".contains(c));
    let host_ok = |h: &str| !h.is_empty() && !h.starts_with(['-', '.']) && h.chars().all(|c| c.is_ascii_alphanumeric() || ".-".contains(c));
    let user_ok = |u: &str| !u.is_empty() && !u.starts_with('-') && u.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c));
    if let Some(rest) = url.strip_prefix("ssh://") {
        let Some((authority, path)) = rest.split_once('/') else { return false };
        let host_port = authority.split_once('@').map_or(authority, |(u, h)| if user_ok(u) { h } else { "" });
        let host = match host_port.split_once(':') {
            Some((h, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => h,
            Some(_) => return false,
            None => host_port,
        };
        return host_ok(host) && safe(path) && !path.contains("..");
    }
    let Some((user, rest)) = url.split_once('@') else { return false };
    let Some((host, path)) = rest.split_once(':') else { return false };
    user_ok(user) && host_ok(host) && safe(path) && !path.contains("..") && !path.starts_with('-')
}

/// The remote's place in the broker's state: its key, the host key pinned, the last push.
pub struct Remote {
    pub url: String,
    /// The machine's: its key, the host keys pinned.
    dir: PathBuf,
    /// This remote's (by its URL): the last push, what was verified, its alarm — a new URL starts
    /// afresh rather than alarming at a history that is another repository's (the review of ADR-036).
    here: PathBuf,
}

/// The last push, as the broker recorded it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pushed {
    /// When it was tried (UTC, RFC 3339).
    pub at: String,
    /// When pushes began to fail, if they are failing (UTC): a day after it, an alarm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_since: Option<String>,
    /// When a push last went through (UTC), whenever that was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ok: Option<String>,
    /// Whether it went through.
    pub ok: bool,
    /// Why `main` was not taken, when the ledger was (another history there, a commit confirmed
    /// waiting, an operation unfinished).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<String>,
    /// Why not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many ledger entries the remote had after it, if it went through.
    #[serde(default)]
    pub entries: usize,
    /// What the remote's ledger said of this one, before the push (a push is not made if it differs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

/// What the remote's ledger says of this one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Anchor {
    /// The remote has the first `remote` of the `local` entries, exactly: the rest are not pushed yet.
    Agrees { remote: usize, local: usize },
    /// The remote has none yet.
    Empty,
    /// The remote's ledger is not the start of this one: this one was rewritten, or cut short.
    Differs { why: String },
    /// It could not be asked.
    Unreachable { error: String },
}

impl Remote {
    pub fn new(url: &str, state_dir: &Path) -> Result<Self> {
        // The paths go into the shell command git runs for ssh, single-quoted: none may hold a quote.
        if state_dir.to_string_lossy().contains(['\'', '\n']) {
            bail!("the broker's state directory has a quote in its name: no remote");
        }
        if !url_ok(url) {
            bail!("the remote must be an SSH URL (ssh://host/path or user@host:path), not {url:?}");
        }
        use sha2::Digest as _;
        let dir = state_dir.join("remote");
        let here = dir.join(&hex::encode(sha2::Sha256::digest(url.as_bytes()))[..16]);
        Ok(Remote { url: url.to_string(), dir, here })
    }

    pub fn key(&self) -> PathBuf {
        self.dir.join("key")
    }

    /// The machine's key for the remote — made the first time, kept after: its public half, to add
    /// to the repository as a deploy key (with write access).
    pub fn public_key(&self, host: &str) -> Result<String> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
        if !self.key().exists() {
            let comment = format!("torq-broker@{host}");
            let ok = crate::config::command("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
                .arg(self.key())
                .status()
                .context("running ssh-keygen")?
                .success();
            if !ok {
                bail!("ssh-keygen could not make the remote's key");
            }
        }
        let p = self.dir.join("key.pub");
        Ok(std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?.trim().to_string())
    }

    /// Forget the host keys pinned: an adoption given up, on a machine with nothing yet to protect.
    pub fn forget_host(&self) -> Result<()> {
        match std::fs::remove_file(self.dir.join("known_hosts")) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }

    /// The fingerprint of the host key pinned, for a person to compare with the host's published one.
    pub fn host_fingerprint(&self) -> Option<String> {
        let out = crate::config::command("ssh-keygen").arg("-lf").arg(self.dir.join("known_hosts")).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.split_whitespace().nth(1)).collect::<Vec<_>>().join(", "))
    }

    /// How git reaches the remote: no ssh configuration of anyone's, this key and no other, the host key
    /// pinned (accepted the first
    /// time, held to after), never a question, and a dead connection given up.
    pub fn ssh_command(&self) -> String {
        format!(
            "ssh -F /dev/null -i '{}' -o IdentitiesOnly=yes -o IdentityAgent=none -o 'UserKnownHostsFile={}' -o StrictHostKeyChecking=accept-new \
             -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3",
            self.key().display(),
            self.dir.join("known_hosts").display()
        )
    }

    /// The remote's ledger commit last checked and found to agree (None: none yet, or forgotten —
    /// then the whole of its history is checked).
    pub fn verified(&self) -> Option<String> {
        let v = std::fs::read_to_string(self.here.join("verified")).ok()?;
        let v = v.trim();
        (v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit())).then(|| v.to_string())
    }

    pub fn set_verified(&self, commit: &str) -> Result<()> {
        write_atomic(&self.here, "verified", commit.as_bytes())
    }

    /// A disagreement with the remote, once found, stays until a person clears it
    /// (`torq-broker remote-clear`, ledgered): a network blip, or a later push, does not make it go.
    pub fn alarm(&self) -> Option<Alarm> {
        serde_json::from_slice(&std::fs::read(self.here.join("alarm.json")).ok()?).ok()
    }

    pub fn raise(&self, why: &str) -> Result<()> {
        if self.alarm().is_some() {
            return Ok(()); // the first one found is the one to look into
        }
        let a = Alarm { at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), why: why.to_string() };
        write_atomic(&self.here, "alarm.json", &serde_json::to_vec(&a)?)
    }

    /// Clear the alarm, and what was verified: the next push checks the remote's whole history again.
    pub fn clear(&self) -> Result<()> {
        for f in ["alarm.json", "verified"] {
            match std::fs::remove_file(self.here.join(f)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    pub fn last(&self) -> Option<Pushed> {
        serde_json::from_slice(&std::fs::read(self.here.join("last.json")).ok()?).ok()
    }

    pub fn record(&self, p: &Pushed) -> Result<()> {
        write_atomic(&self.here, "last.json", &serde_json::to_vec(p)?)
    }
}

/// A disagreement between this machine's ledger and its remote's.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Alarm {
    pub at: String,
    pub why: String,
}

fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    // Written whole, synced, renamed, and the folder synced: an alarm a power cut lost would be one cleared
    // without a person (the review of 0.2.1, L5).
    let tmp = dir.join(format!("{name}.tmp"));
    let mut f = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, dir.join(name)).with_context(|| format!("writing {}", dir.join(name).display()))?;
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

/// Compare the remote's ledger (its text) with this machine's: it must be this one's first lines,
/// exactly.
/// The largest ledger read from a remote, into root's memory: whoever can push there could otherwise
/// make it any size (the review of 0.2.1, L3). The same as adopt's.
pub const MAX_LEDGER: u64 = 64 << 20;

pub fn anchor(remote: Option<&str>, local: &str) -> Anchor {
    let Some(remote) = remote else { return Anchor::Empty };
    let r: Vec<&str> = remote.lines().filter(|l| !l.trim().is_empty()).collect();
    let l: Vec<&str> = local.lines().filter(|l| !l.trim().is_empty()).collect();
    if r.len() > l.len() {
        return Anchor::Differs { why: format!("the remote has {} entries and this machine {}: entries are missing here, or the remote is another machine's", r.len(), l.len()) };
    }
    if let Some(i) = r.iter().zip(&l).position(|(a, b)| a != b) {
        return Anchor::Differs { why: format!("entry {} here and the remote's disagree: one of them was rewritten, or the remote is another machine's", i + 1) };
    }
    Anchor::Agrees { remote: r.len(), local: l.len() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_ssh_urls() {
        for ok in ["git@github.com:netstacks/torq5-config.git", "ssh://git@git.example.org/srv/config.git", "ssh://git@host:2222/c.git", "ssh://host/srv/c", "deploy@10.0.0.2:repos/m1"] {
            assert!(url_ok(ok), "{ok}");
        }
        for bad in [
            "ext::sh -c touch% /tmp/pwned", "file:///srv/config", "/srv/config", "https://github.com/x/y", "http://x/y",
            "fd::3", "git@-oProxyCommand=sh:x", "ssh://-oProxyCommand=sh/x", "git@host:--upload-pack=sh", "git@host:../etc",
            "ssh://git@host:22x/y", "ssh://user;id@host/x", "git@host:a b", "", "ssh://host", "git@host:",
        ] {
            assert!(!url_ok(bad), "{bad}");
        }
    }

    #[test]
    fn the_remote_must_be_the_start_of_the_ledger() {
        let local = "a\nb\nc\n";
        assert_eq!(anchor(None, local), Anchor::Empty);
        assert_eq!(anchor(Some("a\nb\n"), local), Anchor::Agrees { remote: 2, local: 3 });
        assert_eq!(anchor(Some(local), local), Anchor::Agrees { remote: 3, local: 3 });
        assert!(matches!(anchor(Some("a\nX\n"), local), Anchor::Differs { .. }), "rewritten");
        assert!(matches!(anchor(Some("a\nb\nc\nd\n"), local), Anchor::Differs { .. }), "cut short here");
        assert!(matches!(anchor(Some("a\nb\nc\n"), "a\nb\n"), Anchor::Differs { .. }));
    }
}
