//! A release's signature (ADR-028). Before a machine takes a commit of TorqOS's releases, that
//! commit is fetched and git checks it was signed by the release key — the key the machine came
//! with (/etc/torq/release-signers, modules/releases.nix), not anything the releases say. Nix itself
//! checks no signature: this is where a forged or altered release is refused.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// The keys a release may be signed with, as the machine has them.
pub const SIGNERS: &str = "/etc/torq/release-signers";

/// Check commit `rev` of `repo` is signed by a key in `signers`: that commit alone is fetched into a
/// folder of its own and checked by git (with OpenSSH). Returns what git says of the signature.
pub fn check(repo: &str, rev: &str, signers: &Path) -> Result<String> {
    if rev.len() != 40 || !rev.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("'{rev}' is not a commit");
    }
    let dir = tempfile::tempdir().context("a folder to check the release in")?;
    let signers = format!("gpg.ssh.allowedSignersFile={}", signers.display());
    // git with nothing of the caller's environment or configuration (as candidate.rs's): no GIT_*
    // variables, no global or system config, no hooks — nothing that could change what it checks.
    let git = |args: &[&str]| -> Result<String> {
        let mut c = Command::new("git");
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(&k);
            }
        }
        // No OpenPGP or X.509 verifier, ever, and no home of the caller's (a keyring there would be
        // one): git picks its verifier from the signature, not from gpg.format (audit A1).
        let out = c
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("HOME", dir.path())
            .env("GNUPGHOME", dir.path())
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "core.hooksPath=/dev/null", "-c", "gpg.format=ssh", "-c", &signers])
            .args(["-c", "gpg.openpgp.program=false", "-c", "gpg.x509.program=false"])
            .args(args)
            .output()
            .context("running git")?;
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !out.status.success() {
            bail!("{}", said.lines().last().unwrap_or("git failed"));
        }
        Ok(said)
    };
    git(&["init", "-q"])?;
    git(&["fetch", "-q", "--depth=1", "--no-tags", repo, rev]).with_context(|| format!("fetching release {} from {repo}", &rev[..12]))?;
    // Only an SSH signature is the release key's (ADR-028): an OpenPGP or X.509 one is refused
    // before git is asked to verify anything.
    if !ssh_signed(&raw_commit(dir.path(), rev)?) {
        bail!("release {} is not signed by the TorqOS release key ({}): its signature, if any, is not an SSH signature", &rev[..12], SIGNERS);
    }
    let said = git(&["verify-commit", rev]).with_context(|| format!("release {} is not signed by the TorqOS release key ({})", &rev[..12], SIGNERS))?;
    Ok(said.lines().find(|l| l.starts_with("Good")).unwrap_or("signed").to_string())
}

/// Commit `rev`'s object as git stores it, in the repository at `dir`.
fn raw_commit(dir: &Path, rev: &str) -> Result<String> {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(dir)
        .args(["cat-file", "commit", rev])
        .output()
        .context("reading the release commit")?;
    if !out.status.success() {
        bail!("reading release {}: {}", &rev[..12.min(rev.len())], String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a commit object's signature (its `gpgsig` header, before the message) is an SSH one.
fn ssh_signed(commit: &str) -> bool {
    let header = commit.split("\n\n").next().unwrap_or("");
    header.lines().any(|l| l == "gpgsig -----BEGIN SSH SIGNATURE-----")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A releases repository of its own: a commit signed with `key`, one signed with `other`, and
    /// one not signed. Returns the folder and the three commits.
    fn releases(dir: &Path, key: &Path, other: &Path) -> (String, String, String) {
        let repo = dir.join("releases");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=TorqOS release", "-c", "user.email=release@torqos.invalid", "-c", "gpg.format=ssh"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        std::fs::create_dir(&repo).unwrap();
        git(&["init", "-q", "-b", "stable"]);
        let commit = |n: &str, by: Option<&Path>| {
            std::fs::write(repo.join("flake.nix"), n).unwrap();
            git(&["add", "flake.nix"]);
            match by {
                Some(k) => git(&["-c", &format!("user.signingkey={}", k.display()), "commit", "-q", "-S", "-m", n]),
                None => git(&["commit", "-q", "-m", n]),
            };
            git(&["rev-parse", "HEAD"])
        };
        (commit("signed", Some(key)), commit("other", Some(other)), commit("unsigned", None))
    }

    fn key(dir: &Path, name: &str) -> std::path::PathBuf {
        let k = dir.join(name);
        let out = Command::new("ssh-keygen").args(["-q", "-t", "ed25519", "-N", "", "-C", name, "-f"]).arg(&k).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        k
    }

    #[test]
    fn only_a_commit_the_release_key_signed() {
        let dir = tempfile::tempdir().unwrap();
        let (release, other) = (key(dir.path(), "release"), key(dir.path(), "other"));
        let public = std::fs::read_to_string(release.with_extension("pub")).unwrap();
        let signers = dir.path().join("allowed_signers");
        std::fs::write(&signers, format!("torqos-release namespaces=\"git,file\" {}", public.split_whitespace().take(2).collect::<Vec<_>>().join(" "))).unwrap();
        let (good, wrong, unsigned) = releases(dir.path(), &release, &other);
        let repo = format!("file://{}", dir.path().join("releases").display());

        // Each fetched as the tip it was: a releases repository's newest commit.
        let tip = |rev: &str| {
            let out = Command::new("git").env("GIT_CONFIG_GLOBAL", "/dev/null").arg("-C").arg(dir.path().join("releases")).args(["branch", "-f", "check", rev]).output().unwrap();
            assert!(out.status.success());
        };
        tip(&good);
        let said = check(&repo, &good, &signers).unwrap();
        assert!(said.starts_with("Good \"git\" signature for torqos-release"), "{said}");
        tip(&wrong);
        let e = format!("{:#}", check(&repo, &wrong, &signers).unwrap_err());
        assert!(e.starts_with(&format!("release {} is not signed by the TorqOS release key", &wrong[..12])), "{e}");
        tip(&unsigned);
        assert!(format!("{:#}", check(&repo, &unsigned, &signers).unwrap_err()).contains("is not signed by the TorqOS release key"));
        // A commit with an OpenPGP signature is not looked at by any verifier: only an SSH one is
        // the release key's (audit A1: git would have handed it to gpg and the caller's keyring).
        let git_in = |args: &[&str], input: Option<&str>| {
            use std::io::Write;
            let mut c = Command::new("git").env("GIT_CONFIG_GLOBAL", "/dev/null").arg("-C").arg(dir.path().join("releases")).args(args)
                .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
            if let Some(i) = input { c.stdin.take().unwrap().write_all(i.as_bytes()).unwrap(); }
            let out = c.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let tree = git_in(&["rev-parse", &format!("{good}^{{tree}}")], None);
        let pgp = git_in(&["hash-object", "-t", "commit", "-w", "--stdin"], Some(&format!(
            "tree {tree}\nparent {unsigned}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n -----END PGP SIGNATURE-----\n\npgp\n")));
        assert!(!ssh_signed(&raw_commit(&dir.path().join("releases"), &pgp).unwrap()));
        assert!(ssh_signed(&raw_commit(&dir.path().join("releases"), &good).unwrap()));
        tip(&pgp);
        let e = format!("{:#}", check(&repo, &pgp, &signers).unwrap_err());
        assert!(e.contains("is not an SSH signature"), "{e}");
        // A commit that is not there, and something that is not a commit at all.
        assert!(format!("{:#}", check(&repo, &"0".repeat(40), &signers).unwrap_err()).starts_with("fetching release 000000000000"));
        assert_eq!(format!("{:#}", check(&repo, "--upload-pack=x", &signers).unwrap_err()), "'--upload-pack=x' is not a commit");
    }
}
