//! Config files attached in the tree (docs/CONFIG-IS-THE-DEVICE.md): `set services httpd config
//! services/httpd.nix` attaches an ordinary NixOS module file of the config repo at that point, for
//! what is not a value; a place may hold several, as a list holds values. Kept in
//! `config-files.json` (where in the tree → its files), which
//! modules/nixos-settings.nix turns into NixOS `imports`; the files themselves are plain Nix, system
//! tier, shown as their raw diffs before anyone commits them.
//!
//! Pure: it reads and writes the file contents it is given.

use crate::tree::Val;
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};

/// The config repo file that says which config files are attached where.
pub const FILE: &str = "config-files.json";

/// Where in the tree (its words, joined by spaces; "" is the top) → the config repo files attached
/// there.
pub type Attached = BTreeMap<String, BTreeSet<String>>;

/// Every attached file, wherever it is attached.
pub fn files(a: &Attached) -> impl Iterator<Item = &String> {
    a.values().flatten()
}

/// A place's files as `config-files.json` holds them: a list, or (the first form, still read) one name.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Files {
    One(String),
    Many(BTreeSet<String>),
}

/// Whether `rel` may be a config file: a `.nix` file inside the config repo — relative, no `..`, no
/// empty or odd parts, nothing but plain path characters — as modules/nixos-settings.nix requires.
pub fn valid(rel: &str) -> Result<()> {
    let parts: Vec<&str> = rel.split('/').collect();
    let plain = rel.chars().all(|c| c.is_ascii_alphanumeric() || "._-/".contains(c));
    if !rel.ends_with(".nix") || rel.starts_with('/') || !plain || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..") {
        bail!("'{rel}' must be a .nix file inside the config repo (like services/httpd.nix: relative, no ..)");
    }
    // Not the file TorqOS keeps the configuration's own structure in.
    if rel == "flake.nix" {
        bail!("'{rel}' is TorqOS's own (it builds the machine from the tree): attach a file of your own");
    }
    Ok(())
}

/// Read `config-files.json` (if there is one), each entry checked.
pub fn load(bytes: Option<&[u8]>) -> Result<Attached> {
    let Some(b) = bytes else { return Ok(Attached::new()) };
    let read: BTreeMap<String, Files> = serde_json::from_slice(b).with_context(|| format!("{FILE}: not a JSON object of file lists; fix the file (as raw JSON) first"))?;
    let mut a = Attached::new();
    for (at, files) in read {
        // (An empty place is the top of the tree: `set config hardware-configuration.nix`.)
        if at.chars().any(char::is_control) || at.starts_with(' ') || at.ends_with(' ') || at.contains("  ") {
            bail!("{FILE}: an attachment's place is not words of the tree");
        }
        let files = match files {
            Files::One(f) => BTreeSet::from([f]),
            Files::Many(f) => f,
        };
        if files.is_empty() {
            bail!("{FILE}: {}: an empty list (no files: no entry)", if at.is_empty() { "the top" } else { &at });
        }
        for rel in &files {
            valid(rel).with_context(|| format!("{FILE}: {}", if at.is_empty() { "the top" } else { &at }))?;
        }
        a.insert(at, files);
    }
    Ok(a)
}

/// `config-files.json` as TorqOS writes it; None: no attachments, no file.
pub fn render(a: &Attached) -> Option<String> {
    (!a.is_empty()).then(|| format!("{}\n", serde_json::to_string_pretty(a).unwrap_or_default()))
}

/// Each place's attachments as an entry: `<place> config <file>`, or `[ <file> … ]` for several.
pub fn entries(a: &Attached) -> Vec<(Vec<String>, Val)> {
    a.iter()
        .map(|(at, files)| {
            let mut w: Vec<String> = at.split(' ').filter(|w| !w.is_empty()).map(String::from).collect();
            w.push("config".into());
            let v = match files.iter().collect::<Vec<_>>().as_slice() {
                [one] => Val::One((*one).clone()),
                many => Val::Many(many.iter().map(|f| (*f).clone()).collect()),
            };
            (w, v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nix_files_inside_the_repo() {
        for ok in ["services/httpd.nix", "custom_apps/my-app.nix", "a.nix"] {
            valid(ok).unwrap_or_else(|e| panic!("{ok}: {e:#}"));
        }
        for bad in ["../x.nix", "/etc/x.nix", "a/../b.nix", "notes.txt", "./a.nix", "a//b.nix", "a b.nix", "flake.nix", ""] {
            assert!(valid(bad).is_err(), "{bad}");
        }
        assert!(load(Some(br#"{"x": "../y.nix"}"#)).is_err());
        assert!(load(Some(br#"["a.nix"]"#)).is_err());
        let a = load(Some(br#"{"services httpd": "services/httpd.nix"}"#)).unwrap();
        assert_eq!(entries(&a), [(vec!["services".into(), "httpd".into(), "config".into()], Val::One("services/httpd.nix".into()))]);
        assert_eq!(load(render(&a).as_deref().map(str::as_bytes)).unwrap(), a, "reads back");
        assert_eq!(render(&Attached::new()), None);
        // At the top of the tree: an empty place, read back (and hardware-configuration.nix may be one).
        let top = load(Some(br#"{"": "hardware-configuration.nix"}"#)).unwrap();
        assert_eq!(entries(&top), [(vec!["config".into()], Val::One("hardware-configuration.nix".into()))]);
        assert!(load(Some(br#"{" services": "a.nix"}"#)).is_err());
        // Several at one place: a list, written as one; the first form (one name) reads as a list of one.
        let two = load(Some(br#"{"": ["hardware-configuration.nix", "custom/motd.nix"]}"#)).unwrap();
        assert_eq!(entries(&two), [(vec!["config".into()], Val::Many(vec!["custom/motd.nix".into(), "hardware-configuration.nix".into()]))]);
        assert_eq!(files(&two).count(), 2);
        assert_eq!(load(render(&two).as_deref().map(str::as_bytes)).unwrap(), two, "reads back");
        assert!(render(&top).is_some_and(|r| r.contains("[")), "written as a list");
        assert!(load(Some(br#"{"": []}"#)).is_err(), "no files: no entry");
        assert!(load(Some(br#"{"": ["a.nix", "../b.nix"]}"#)).is_err(), "every one checked");
    }
}
