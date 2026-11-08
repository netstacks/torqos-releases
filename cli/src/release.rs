//! A release's signature (ADR-028). Before a machine takes a commit of TorqOS's releases, that
//! commit is fetched and git checks it was signed by the release key — the key the machine came
//! with (/etc/torq/release-signers, modules/releases.nix), not anything the releases say. Nix itself
//! checks no signature: this is where a forged or altered release is refused.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// The keys a release may be signed with, as the machine has them.
pub const SIGNERS: &str = "/etc/torq/release-signers";

/// A release whose signature was checked: what git says of it, and its version (audit A3).
#[derive(Debug, Clone, PartialEq)]
pub struct Signed {
    pub said: String,
    /// TorqOS's version at that commit, read from the commit fetched to check it (no second fetch).
    /// None: it has none, or not one in a form compared here.
    pub version: Option<Version>,
}

/// A release's version. The source says only its number (`cli/Cargo.toml`: a pre-release, `rc1`, is
/// the tag's, not the source's); the release workflow names the whole tag in the signed commit's
/// message (`TorqOS v0.3.0-rc1`). Where the message names it, for that number, it is the version,
/// `exact`; otherwise the number alone, which may be a release or a pre-release of it.
#[derive(Debug, Clone, PartialEq)]
pub struct Version {
    pub number: String,
    pub exact: bool,
}

/// A release's version, from its source's `cli/Cargo.toml` and its signed commit's message.
pub fn release_version(cargo_toml: &str, message: &str) -> Option<Version> {
    let number = package_version(cargo_toml)?;
    let tagged = message.lines().next()?.strip_prefix("TorqOS v").filter(|t| is_version(t) && core(t) == number);
    Some(match tagged {
        Some(t) => Version { number: t.to_string(), exact: true },
        None => Version { number, exact: false },
    })
}

/// A version's three numbers, without its pre-release (`0.3.0-rc1` → `0.3.0`).
pub fn core(v: &str) -> &str {
    v.split('-').next().unwrap_or(v)
}

/// Where a TorqOS source says its version (scripts/bump-version sets it; release.yml checks the tag
/// is it).
const VERSION_FILE: &str = "cli/Cargo.toml";

/// Check commit `rev` of `repo` is signed by a key in `signers`: that commit alone is fetched into a
/// folder of its own and checked by git (with OpenSSH). Returns what git says of the signature, and
/// the version of TorqOS the commit holds.
pub fn check(repo: &str, rev: &str, signers: &Path) -> Result<Signed> {
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
    let raw = raw_commit(dir.path(), rev)?;
    if !ssh_signed(&raw) {
        bail!("release {} is not signed by the TorqOS release key ({}): its signature, if any, is not an SSH signature", &rev[..12], SIGNERS);
    }
    let said = git(&["verify-commit", rev]).with_context(|| format!("release {} is not signed by the TorqOS release key ({})", &rev[..12], SIGNERS))?;
    // Read only once the signature holds: the version is the signed commit's own.
    let message = raw.split_once("\n\n").map_or("", |(_, m)| m);
    let version = git_out(dir.path(), &["cat-file", "blob", &format!("{rev}:{VERSION_FILE}")]).ok().and_then(|t| release_version(&t, message));
    Ok(Signed { said: said.lines().find(|l| l.starts_with("Good")).unwrap_or("signed").to_string(), version })
}

/// The `version` of a Cargo.toml's `[package]` section, as release.yml reads it, if it is a version
/// (`0.2.1`, `0.3.0-rc1`): anything else is neither shown nor compared.
pub fn package_version(toml: &str) -> Option<String> {
    let mut in_package = false;
    for line in toml.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
        } else if let Some(v) = line.strip_prefix("version").filter(|_| in_package).and_then(|r| r.trim_start().strip_prefix('=')) {
            let v = v.trim().strip_prefix('"')?.strip_suffix('"')?;
            return is_version(v).then(|| v.to_string());
        }
    }
    None
}

/// The version of TorqOS an os-release file says the machine runs (modules/branding.nix: `ID=torqos`,
/// `VERSION_ID` torqcli's version): None where it is not TorqOS's (a machine from before ADR-029
/// says NixOS's, which is not TorqOS's version).
pub fn os_release_version(text: &str) -> Option<String> {
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix('=')).map(|v| v.trim().trim_matches('"').to_string());
    (field("ID")? == "torqos").then(|| field("VERSION_ID")).flatten().filter(|v| is_version(v))
}

/// Whether `v` is a version in the form compared here (`0.2.1`, `0.3.0-rc1`): nothing else is shown.
pub fn is_version(v: &str) -> bool {
    parse(v).is_some()
}

/// A version as compared: its three numbers, and its pre-release (`rc1`; None: a release).
fn parse(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) if !p.is_empty() && p.split('.').all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric())) => (c, Some(p)),
        Some(_) => return None,
        None => (v, None),
    };
    let number = |p: &str| (!p.is_empty() && p.len() <= 9 && p.chars().all(|c| c.is_ascii_digit())).then(|| p.parse::<u64>().ok()).flatten();
    let parts: Vec<&str> = core.split('.').collect();
    match parts[..] {
        [a, b, c] => Some(([number(a)?, number(b)?, number(c)?], pre)),
        _ => None,
    }
}

/// How version `a` stands to `b`, as SemVer orders them: a pre-release before its release, two
/// pre-releases part by part (numbers as numbers, before words). None: either is not a version.
pub fn order(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let ((na, pa), (nb, pb)) = (parse(a)?, parse(b)?);
    Some(na.cmp(&nb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let part = |p: &str| p.parse::<u64>().map_or((1, 0, p.to_string()), |n| (0, n, String::new()));
            x.split('.').map(part).cmp(y.split('.').map(part))
        }
    }))
}

/// Commit `rev`'s object as git stores it, in the repository at `dir`.
fn raw_commit(dir: &Path, rev: &str) -> Result<String> {
    git_out(dir, &["cat-file", "commit", rev]).with_context(|| format!("reading release {}", &rev[..12.min(rev.len())]))
}

/// What git prints for `args`, in the repository at `dir` (the commit fetched to check it), with no
/// configuration of the caller's.
fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
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
        // The signed release says its version where every TorqOS source does.
        std::fs::create_dir(repo.join("cli")).unwrap();
        std::fs::write(repo.join("cli/Cargo.toml"), "[package]\nname = \"torq\"\nversion = \"0.2.2\"\n\n[dependencies]\nversion = \"9\"\n").unwrap();
        git(&["add", "cli/Cargo.toml"]);
        let commit = |n: &str, by: Option<&Path>| {
            std::fs::write(repo.join("flake.nix"), n).unwrap();
            git(&["add", "flake.nix"]);
            match by {
                Some(k) => git(&["-c", &format!("user.signingkey={}", k.display()), "commit", "-q", "-S", "-m", n]),
                None => git(&["commit", "-q", "-m", n]),
            };
            git(&["rev-parse", "HEAD"])
        };
        (commit("TorqOS v0.2.2", Some(key)), commit("other", Some(other)), commit("unsigned", None))
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
        let signed = check(&repo, &good, &signers).unwrap();
        assert_eq!(signed.version, Some(Version { number: "0.2.2".into(), exact: true }), "read from the commit fetched to check it: its source and its message");
        let said = signed.said;
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

    /// A source's version is its `[package]` section's, and only a version in SemVer's form is one;
    /// versions are ordered as SemVer orders them, never as text or by date (audit A3).
    #[test]
    fn versions_read_and_ordered() {
        assert_eq!(package_version("[package]\nname = \"torq\"\nversion = \"0.2.1\"\n").as_deref(), Some("0.2.1"));
        assert_eq!(package_version("[package]\nversion=\"0.3.0-rc1\"").as_deref(), Some("0.3.0-rc1"));
        assert_eq!(package_version("[dependencies]\nversion = \"1.0.0\"\n[package]\nname = \"x\"\n"), None, "another section's version is not the source's");
        assert_eq!(package_version("[package]\nversions = \"1.0.0\"\n"), None);
        for bad in ["1.2", "1.2.3.4", "v1.2.3", "1.2.x", "1.2.3-", "1.2.3-rc..1", "1.2.3-r c", "1.2.3\\u001b[2J", "1234567890.0.0"] {
            assert_eq!(package_version(&format!("[package]\nversion = \"{bad}\"\n")), None, "{bad}");
        }
        use std::cmp::Ordering::*;
        for (a, b, want) in [
            ("0.2.1", "0.2.2", Less),
            ("0.10.0", "0.9.9", Greater),
            ("0.2.0", "0.2.0", Equal),
            ("0.3.0-rc1", "0.3.0", Less),
            ("0.3.0-rc.2", "0.3.0-rc.10", Less),
            ("0.3.0-1", "0.3.0-rc", Less),
            ("0.1.2", "0.2.0", Less),
        ] {
            assert_eq!(order(a, b), Some(want), "{a} against {b}");
        }
        assert_eq!(order("0.2.0", "26.05"), None);
        // The tag the signed commit's message names, for the source's number, is the version: a
        // pre-release says so. A message without one, or naming another number: the number alone.
        let toml = "[package]\nversion = \"0.3.0\"\n";
        assert_eq!(release_version(toml, "TorqOS v0.3.0-rc1\n"), Some(Version { number: "0.3.0-rc1".into(), exact: true }));
        assert_eq!(release_version(toml, "TorqOS v0.3.0"), Some(Version { number: "0.3.0".into(), exact: true }));
        assert_eq!(release_version(toml, "TorqOS v0.4.0-rc1"), Some(Version { number: "0.3.0".into(), exact: false }), "not this source's number");
        assert_eq!(release_version(toml, "some commit"), Some(Version { number: "0.3.0".into(), exact: false }));
        assert_eq!(release_version("[package]\n", "TorqOS v0.3.0"), None, "no number in the source: none");
        assert_eq!(core("0.3.0-rc.1"), "0.3.0");
        // The running version is TorqOS's own, never NixOS's.
        assert_eq!(os_release_version("NAME=TorqOS\nID=torqos\nID_LIKE=nixos\nVERSION_ID=\"0.2.1\"\n").as_deref(), Some("0.2.1"));
        assert_eq!(os_release_version("NAME=NixOS\nID=nixos\nVERSION_ID=\"26.05\"\n"), None);
        assert_eq!(os_release_version("ID=torqos\nVERSION_ID=weird\n"), None);
    }
}
