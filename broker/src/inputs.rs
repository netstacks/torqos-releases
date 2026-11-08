//! What a changed flake.lock builds, checked before it is built (the audit of 2026-11-04, A2). Nix
//! looks a locked input up by its `narHash`: a lock can name a `rev` the release key signed and the
//! hash of other source already in the store, and Nix builds that source. So each input the lock
//! changes is fetched again by what names it (its `rev`), its hash recomputed and compared; and
//! TorqOS's own releases are checked to be signed by the release key, here, as root, not only by the
//! torqcli that wrote the lock. A folder (`path:`, dev mode only) is hashed again too.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The keys a release may be signed with, as the machine has them (modules/releases.nix).
pub const SIGNERS: &str = "/etc/torq/release-signers";
/// Where this machine takes its releases from (modules/releases.nix).
pub const RELEASES: &str = "/etc/torq/releases.json";

/// The locked inputs `new` has that `old` does not have as they are: by node name.
pub fn changed(old: &str, new: &str) -> Result<BTreeMap<String, Value>> {
    let parse = |t: &str| -> Result<Value> { if t.trim().is_empty() { Ok(Value::Null) } else { Ok(serde_json::from_str(t)?) } };
    let (old, new) = (parse(old).context("reading the old flake.lock")?, parse(new).context("reading the new flake.lock")?);
    let mut out = BTreeMap::new();
    for (name, node) in new["nodes"].as_object().into_iter().flatten() {
        let locked = &node["locked"];
        if locked.is_null() || old["nodes"][name]["locked"] == *locked {
            continue;
        }
        out.insert(name.clone(), locked.clone());
    }
    Ok(out)
}

/// The release repository's address, as git fetches it (`https://…`), from the machine's setting.
pub fn releases_repo(settings: &str) -> Option<String> {
    let v: Value = serde_json::from_str(settings).ok()?;
    let url = v["url"].as_str()?.strip_prefix("git+")?;
    Some(url.split('?').next()?.trim_end_matches('/').to_string())
}

/// Whether a locked git input is the machine's releases (so must be signed by the release key).
pub fn is_release(locked: &Value, releases: Option<&str>) -> bool {
    let norm = |u: &str| u.trim_end_matches('/').trim_end_matches(".git").to_string();
    locked["type"] == "git" && releases.is_some_and(|r| locked["url"].as_str().map(norm) == Some(norm(r)))
}

/// The inputs to fetch again, as `builtins.fetchTree` takes them: what locked them, but the hash, which
/// is what is checked. A folder (dev mode's `path:`) is hashed again too.
pub fn to_fetch(changes: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    changes
        .iter()
        // A folder too: fetched again it is hashed again, and a lock naming a store path for a folder is
        // that folder's hash or refused (the audit of 0.3.0: it was skipped, whatever path it named).
        .map(|(n, l)| {
            let mut a = l.clone();
            if let Some(o) = a.as_object_mut() {
                // What is checked, and what Nix derives itself from what it fetches: given back, they
                // would have to match too, and say nothing about what is built.
                for k in ["narHash", "lastModified", "lastModifiedDate", "revCount"] {
                    o.remove(k);
                }
            }
            (n.clone(), a)
        })
        .collect()
}

/// The expression that computes, for `to_fetch` written to `file`, each one's narHash as fetched now.
/// (`nix eval` applies no `--arg` to an expression: the path is in it, as a string.)
pub fn expr(file: &Path) -> String {
    let path = serde_json::to_string(&file.to_string_lossy()).unwrap_or_default();
    format!("builtins.mapAttrs (n: a: (builtins.fetchTree a).narHash) (builtins.fromJSON (builtins.readFile {path}))")
}

/// Those whose hash, fetched again, is not the lock's.
pub fn mismatched(changes: &BTreeMap<String, Value>, fetched: &Value) -> Vec<String> {
    to_fetch(changes)
        .keys()
        .filter(|n| fetched[n.as_str()].as_str().is_none() || fetched[n.as_str()] != changes[n.as_str()]["narHash"])
        .cloned()
        .collect()
}

/// Whether the raw commit object carries an SSH signature (`-----BEGIN SSH SIGNATURE-----` in its
/// gpgsig header): OpenPGP and X.509 ones are not the release key's (the audit of 2026-11-04, A1).
pub fn ssh_signed(commit: &str) -> bool {
    let mut header = true;
    let mut sig = false;
    for line in commit.lines() {
        if line.is_empty() {
            header = false;
        }
        if header && line.starts_with("gpgsig ") {
            sig = line.trim_end() == "gpgsig -----BEGIN SSH SIGNATURE-----";
        }
    }
    sig
}

/// The arguments for git, with nothing of anyone's configuration, that check `rev` is signed by a key
/// in `signers`.
pub fn git_config(signers: &Path) -> Vec<String> {
    [
        "-c", "core.hooksPath=/dev/null", "-c", "gpg.format=ssh",
        "-c", "gpg.openpgp.program=false", "-c", "gpg.x509.program=false",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain(["-c".to_string(), format!("gpg.ssh.allowedSignersFile={}", signers.display())])
    .collect()
}

/// A rev: forty hex digits.
pub fn rev_ok(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lock(nixpkgs_hash: &str, torq: Value) -> String {
        json!({ "nodes": {
            "nixpkgs": { "locked": { "type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "a".repeat(40), "narHash": nixpkgs_hash } },
            "torq": { "locked": torq },
            "root": { "inputs": { "torq": "torq" } }
        }, "root": "root", "version": 7 })
        .to_string()
    }

    #[test]
    fn what_changed_and_what_is_fetched_again() {
        let torq = json!({ "type": "git", "url": "https://github.com/netstacks/torqos-releases", "rev": "b".repeat(40), "narHash": "sha256-t" });
        let old = lock("sha256-n", torq.clone());
        assert!(changed(&old, &old).unwrap().is_empty());
        let new = lock("sha256-n2", torq.clone());
        let c = changed(&old, &new).unwrap();
        assert_eq!(c.keys().collect::<Vec<_>>(), ["nixpkgs"]);
        let f = to_fetch(&c);
        assert!(f["nixpkgs"].get("narHash").is_none() && f["nixpkgs"]["rev"] == "a".repeat(40));
        assert_eq!(mismatched(&c, &json!({ "nixpkgs": "sha256-n2" })), Vec::<String>::new());
        assert_eq!(mismatched(&c, &json!({ "nixpkgs": "sha256-older" })), ["nixpkgs"]);
        assert_eq!(mismatched(&c, &json!({})), ["nixpkgs"], "not fetched is not agreed");
        // A folder, dev mode's: hashed again too, so a lock naming a store path for it is checked.
        let dev = lock("sha256-n", json!({ "type": "path", "path": "/home/x/torq", "narHash": "sha256-p" }));
        let c = changed(&old, &dev).unwrap();
        assert_eq!(to_fetch(&c).keys().collect::<Vec<_>>(), ["torq"]);
        assert_eq!(mismatched(&c, &json!({ "torq": "sha256-p" })), Vec::<String>::new());
        assert_eq!(mismatched(&c, &json!({})), ["torq"], "a folder not hashed again is not agreed");
        // A new machine's first lock: everything is new.
        assert_eq!(changed("", &old).unwrap().len(), 2);
    }

    #[test]
    fn the_releases_and_their_signature() {
        let r = releases_repo(r#"{"url":"git+https://github.com/netstacks/torqos-releases?ref=stable"}"#);
        assert_eq!(r.as_deref(), Some("https://github.com/netstacks/torqos-releases"));
        assert!(is_release(&json!({ "type": "git", "url": "https://github.com/netstacks/torqos-releases.git" }), r.as_deref()));
        assert!(!is_release(&json!({ "type": "github", "owner": "netstacks", "repo": "torqos" }), r.as_deref()));
        assert!(!is_release(&json!({ "type": "git", "url": "https://example.com/other" }), r.as_deref()));
        assert!(ssh_signed("tree t\ngpgsig -----BEGIN SSH SIGNATURE-----\n x\n -----END SSH SIGNATURE-----\n\nmsg\n"));
        assert!(!ssh_signed("tree t\ngpgsig -----BEGIN PGP SIGNATURE-----\n x\n\nmsg\n"));
        assert!(!ssh_signed("tree t\n\ngpgsig -----BEGIN SSH SIGNATURE-----\n"), "in the message is not a signature");
        assert!(rev_ok(&"c".repeat(40)) && !rev_ok("main") && !rev_ok(&"g".repeat(40)));
    }
}
