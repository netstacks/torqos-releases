//! `system software`: where this machine's TorqOS comes from (docs/CONFIG-IS-THE-DEVICE.md, S4b).
//! The source is the one `torq.url = "…";` line of TorqOS's fixed flake.nix; changing it rewrites
//! that line and, in the same candidate, locks the new source (`flake.lock`), so a person approves
//! both together. The version is the lock's: `request system update`, never `set`.
//!
//! Pure: it reads and rewrites the flake.nix text it is given.

use anyhow::{bail, Result};

/// The setting's path in the tree.
pub const PATH: [&str; 4] = ["system", "software", "torq", "source"];

/// The source a fixed flake.nix names (its single `torq.url = "…";` line), if it is one.
pub fn source_of(flake: &str) -> Option<String> {
    let mut found = flake.lines().filter_map(|l| l.trim().strip_prefix("torq.url = \"")?.strip_suffix("\";"));
    let one = found.next()?;
    (found.next().is_none() && valid(one).is_ok()).then(|| one.to_string())
}

/// `flake` with its source set to `url` (indentation kept; nothing else changes).
pub fn with_source(flake: &str, url: &str) -> Result<String> {
    valid(url)?;
    if source_of(flake).is_none() {
        bail!("this machine's flake.nix is not TorqOS's fixed one (one `torq.url = \"…\";` line): its source cannot be set here");
    }
    // Only the line source_of read (the same test), each line's ending kept as it was.
    let is_source = |l: &str| l.trim().strip_prefix("torq.url = \"").and_then(|r| r.strip_suffix("\";")).is_some_and(|s| valid(s).is_ok());
    Ok(flake
        .split_inclusive('\n')
        .map(|l| {
            let body = l.trim_end_matches(['\n', '\r']);
            if !is_source(body) {
                return l.to_string();
            }
            let indent = &body[..body.len() - body.trim_start().len()];
            format!("{indent}torq.url = \"{url}\";{}", &l[body.len()..])
        })
        .collect())
}

/// Whether `url` is a folder someone can write to (`path:`, outside the read-only store): whoever
/// can write there decides what the next build installs, so it is for developing TorqOS only (dev
/// mode). A path in the store cannot change — an image's own TorqOS (docs/INSTALL.md) — and is
/// any machine's.
pub fn writable_folder(url: &str) -> bool {
    url.strip_prefix("path:").is_some_and(|p| !store_path(p))
}

/// Exactly one store path, `/nix/store/<hash>-<name>`, and nothing after it: no `/`, no `%` (Nix
/// decodes `%2e%2e` to `..`), no `..` — the form Nix itself gives, which cannot lead anywhere else.
fn store_path(p: &str) -> bool {
    let Some((hash, name)) = p.strip_prefix("/nix/store/").and_then(|r| r.split_once('-')) else { return false };
    hash.len() == 32
        && hash.chars().all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
        && !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "+._?=-".contains(c))
}

/// The repository of TorqOS's releases (ADR-028), as git fetches it and a lock names it, from the
/// machine's releases address (`git+https://…/torqos-releases?ref=stable`, /etc/torq/releases.json):
/// each release a commit the release key signed, taken as git, so what is built is exactly the
/// commit whose signature was checked.
pub fn releases_repo(url: &str) -> String {
    let bare = url.strip_prefix("git+").unwrap_or(url);
    normal(bare.split('?').next().unwrap_or(bare))
}

/// A repository's address as GitHub and git take it alike: no `.git`, no `/` after it, any case.
fn normal(repo: &str) -> String {
    repo.trim_end_matches('/').trim_end_matches(".git").to_ascii_lowercase()
}

/// Whether `url` is an image's own TorqOS (`path:` a store path): what a machine set up from the
/// image starts from, until its first update moves it to the releases.
pub fn image_own(url: &str) -> bool {
    url.strip_prefix("path:").is_some_and(store_path)
}

/// The commit a flake.lock takes TorqOS at, if it is one of the releases' (`repo`, from
/// releases_repo; as git, or as `github:<owner>/<repo>`): a commit whose signature is checked
/// before it is loaded.
pub fn release_commit(lock: &str, repo: &str) -> Option<String> {
    let lock: serde_json::Value = serde_json::from_str(lock).ok()?;
    let node = lock["nodes"]["root"]["inputs"]["torq"].as_str()?;
    let locked = &lock["nodes"][node]["locked"];
    let releases = match locked["type"].as_str()? {
        "git" => normal(locked["url"].as_str()?) == normal(repo),
        "github" => normal(&format!("https://github.com/{}/{}", locked["owner"].as_str()?, locked["repo"].as_str()?)) == normal(repo),
        _ => false,
    };
    releases.then(|| locked["rev"].as_str().map(String::from)).flatten()
}

/// Whether `url` may be a source: a flake reference of a kind TorqOS can be fetched from, written
/// with plain URL characters only (it is put inside a Nix string: no quote, backslash or `${`).
pub fn valid(url: &str) -> Result<()> {
    let plain = !url.is_empty() && url.chars().all(|c| c.is_ascii_alphanumeric() || ":/._~?&=%+@-".contains(c));
    let kinds = ["github:", "gitlab:", "sourcehut:", "git+https://", "https://", "path:/"];
    if !plain || !kinds.iter().any(|k| url.starts_with(k) && url.len() > k.len()) || url.contains("..") {
        bail!(
            "'{url}' is not a TorqOS source: one of github:<owner>/<repo>[/<ref>], gitlab:…, sourcehut:…, \
             git+https://…, https://… (a tarball) or path:/<absolute folder>"
        );
    }
    // A query (`ref=stable`) with no `?` before it: at the prompt `?` lists what may come next
    // instead of being typed, so a pasted address loses it. No repository or host has `=` in its
    // name; a folder may (`path:`).
    if let Some(eq) = url.find('=').filter(|_| !url.starts_with("path:") && !url.contains('?')) {
        // Where the `?` was: before the name of the query Nix reads (`ref`, `rev`, `dir`, …).
        let names = ["ref", "rev", "dir", "host", "narHash", "submodules", "shallow", "allRefs", "lfs"];
        let fixed = names.iter().find(|n| url[..eq].ends_with(*n)).map(|n| format!("{}?{}", &url[..eq - n.len()], &url[eq - n.len()..]));
        bail!(
            "'{url}' has a query (`=`) but no `?` before it: at the prompt, `?` lists what may come next \
             instead of typing it. Put the address in quotes: \"{}\"",
            fixed.as_deref().unwrap_or("…?…")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAKE: &str = "{\n  inputs = {\n    # The TorqOS distribution.\n    torq.url = \"github:netstacks/torqos\";\n    nixpkgs.follows = \"torq/nixpkgs\";\n  };\n}\n";

    #[test]
    fn only_a_folder_that_can_change_is_dev_only() {
        let store = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";
        assert!(!writable_folder(&format!("path:{store}")) && !writable_folder("github:netstacks/torqos"));
        for folder in [
            "path:/home/viper/torq".to_string(),
            "path:/nix/store/../home/x".into(),
            "path:/nix/store/%2e%2e/%2e%2e/home/u/torq".into(),
            format!("path:{store}/../../home/x"),
            format!("path:{store}/sub"),
            "path:/nix/store/short-source".into(),
            "path:/nix/store/0123456789abcdfghijklmnpqrsvwxyz-%2e%2e".into(),
            "path:/nix/store/0123456789ABCDFGHIJKLMNPQRSVWXYZ-source".into(),
        ] {
            assert!(writable_folder(&folder), "{folder}: only exactly a store path is any machine's");
        }
    }

    #[test]
    fn the_source_is_the_one_line() {
        assert_eq!(source_of(FLAKE).as_deref(), Some("github:netstacks/torqos"));
        let moved = with_source(FLAKE, "path:/home/dev/torq").unwrap();
        assert_eq!(moved, FLAKE.replace("github:netstacks/torqos", "path:/home/dev/torq"), "only that line changes");
        assert_eq!(source_of(&moved).as_deref(), Some("path:/home/dev/torq"));
        // Not TorqOS's fixed flake.nix: no source (two lines, none, or one that is not a source).
        assert_eq!(source_of(&format!("{FLAKE}    torq.url = \"github:a/b\";\n")), None);
        assert_eq!(source_of("{ }"), None);
        assert_eq!(source_of(&FLAKE.replace("github:netstacks/torqos", "${evil}")), None);
        assert!(with_source("{ }", "github:a/b").is_err());
        // Only that line (not a commented copy), and the file's line endings kept.
        let both = FLAKE.replace("    nixpkgs", "    # torq.url = \"github:old/one\"; was\n    nixpkgs");
        assert_eq!(with_source(&both, "github:a/b").unwrap(), both.replace("netstacks/torqos", "a/b"));
        let crlf = FLAKE.replace('\n', "\r\n");
        assert_eq!(with_source(&crlf, "github:a/b").unwrap(), crlf.replace("netstacks/torqos", "a/b"));
    }

    #[test]
    fn only_plain_sources() {
        for ok in ["github:netstacks/torqos", "github:netstacks/torqos/v0.1.0", "gitlab:a/b", "git+https://example.com/torq.git?ref=main", "https://example.com/torq-0.1.tar.gz", "path:/home/viper/torq"] {
            valid(ok).unwrap_or_else(|e| panic!("{ok}: {e:#}"));
        }
        for bad in ["", "github:", "path:relative", "path:/a/../b", "file:///etc", "github:a/b\";x", "github:a/${b}", "github:a/b c", "github:a\\b", "/home/x", "git+ssh://h/r", "github:a/b\n"] {
            assert!(valid(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_releases_commit_a_lock_names() {
        const RELEASES: &str = "git+https://github.com/netstacks/torqos-releases?ref=stable";
        let repo = releases_repo(RELEASES);
        assert_eq!(repo, "https://github.com/netstacks/torqos-releases");
        assert_eq!(releases_repo("git+https://Example.com/Torq.git/?ref=main"), "https://example.com/torq", "as a lock may name it");
        let lock = |locked: &str| format!(r#"{{"nodes":{{"root":{{"inputs":{{"torq":"torq_2","nixpkgs":"nixpkgs"}}}},"torq_2":{{"locked":{locked}}}}},"root":"root","version":7}}"#);
        let git = lock(r#"{"type":"git","url":"https://github.com/netstacks/torqos-releases","ref":"refs/heads/stable","rev":"abc123"}"#);
        assert_eq!(release_commit(&git, &repo).as_deref(), Some("abc123"));
        let gh = lock(r#"{"type":"github","owner":"NetStacks","repo":"torqos-releases","rev":"def456"}"#);
        assert_eq!(release_commit(&gh, &repo).as_deref(), Some("def456"), "the tarball form names a commit too; GitHub is case-blind");
        let dotgit = lock(r#"{"type":"git","url":"https://github.com/netstacks/torqos-releases.git/","rev":"abc123"}"#);
        assert_eq!(release_commit(&dotgit, &repo).as_deref(), Some("abc123"));
        for other in [
            lock(r#"{"type":"git","url":"https://github.com/netstacks/torqos","rev":"abc123"}"#),
            lock(r#"{"type":"git","url":"https://github.com/netstacks/torqos-releases-evil","rev":"abc123"}"#),
            lock(r#"{"type":"path","path":"/nix/store/x-source"}"#),
            "not json".into(),
        ] {
            assert_eq!(release_commit(&other, &repo), None, "{other}");
        }
        assert!(image_own("path:/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source") && !image_own("path:/home/viper/torq") && !image_own(RELEASES));
        valid(RELEASES).unwrap();
    }

    /// A query whose `?` was taken by the prompt (`?` lists choices there): refused, with the address
    /// as it was meant, quoted.
    #[test]
    fn a_query_that_lost_its_question_mark() {
        let e = format!("{:#}", valid("git+https://github.com/netstacks/torqos-releasesref=stable").unwrap_err());
        assert_eq!(
            e,
            "'git+https://github.com/netstacks/torqos-releasesref=stable' has a query (`=`) but no `?` before it: at the prompt, `?` lists what may come next instead of typing it. Put the address in quotes: \"git+https://github.com/netstacks/torqos-releases?ref=stable\""
        );
        assert!(format!("{:#}", valid("https://example.com/xa=b").unwrap_err()).ends_with("in quotes: \"…?…\""), "a query Nix does not name: no guess");
        valid("github:netstacks/torqos?dir=sub").unwrap();
        valid("path:/home/dev/a=b").unwrap();
    }
}
