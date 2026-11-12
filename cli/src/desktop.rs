//! A person's desktop, saved (ADR-045 4, amended 2026-11-10): COSMIC keeps one small file per setting
//! under `~/.config/cosmic/<component>/v<n>/<key>`; they are kept in the person's own configuration,
//! `users/<name>/desktop.json` (user tier, no password), so a machine rebuilt from the repository comes
//! back looking the same, and an undo or a pull puts the desktop back as it was.
//!
//! Three versions of each setting: the configuration's (saved), this desktop's (local), and the one both
//! last agreed on (applied, kept in `~/.local/state/torq`). What changed since they agreed tells which
//! side is newer: the person's change is saved, the configuration's is put on the desktop. Settings of
//! this machine rather than the person (which display the panel is on, when to sleep) are left out.

use crate::candidate::Repo;
use crate::session::Machine;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The leaf in a person's folder (broker/user-leaves.json).
pub const FILE: &str = "desktop.json";

// What the broker accepts (broker/src/policy.rs mirrors these): what it would refuse is never sent.
pub const MAX_VALUE: usize = 64 * 1024;
pub const MAX_KEYS: usize = 2000;
pub const MAX_TOTAL: usize = 1024 * 1024;

/// When a person's desktop is saved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Saving {
    /// By itself, as the person changes it.
    #[default]
    Automatic,
    /// Only when they ask (`request system desktop save`).
    Manual,
}

/// `users/<name>/desktop.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    #[serde(default)]
    pub save: Saving,
    /// Each COSMIC setting, `<component>/v<n>/<key>`, as COSMIC wrote it.
    #[serde(default)]
    pub cosmic: BTreeMap<String, String>,
}

impl Saved {
    fn render(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }
}

/// The settings kept: COSMIC's own components only, named as COSMIC lays them out. One rule with the broker's,
/// which refuses the rest (torq_wire::desktop).
pub use torq_wire::desktop::{key_ok, COSMIC};

pub fn value_ok(v: &str) -> bool {
    v.len() <= MAX_VALUE && !v.contains('\0')
}

/// This machine's, not the person's: which display the panel and dock are on, and when it sleeps (the
/// hardware decides that: some laptops cannot wake their display).
pub fn machine_specific(k: &str) -> bool {
    let component = k.split('/').next().unwrap_or_default();
    let key = k.rsplit('/').next().unwrap_or_default();
    (component.starts_with("com.system76.CosmicPanel.") && key == "output") || (component == "com.system76.CosmicIdle" && key.starts_with("suspend_"))
}

/// What a COSMIC program writes of its own accord, not a setting the person chose: kept, it would make a save of
/// nothing. By observation (docs/KNOWN-GAPS.md 6zx): the screenshot tool's last area and display, rewritten at
/// each screenshot.
pub fn programs_own(k: &str) -> bool {
    k == "com.system76.CosmicPortal/v1/screenshot"
}

/// Whether setting `k` may be in a person's saved desktop at all.
fn keepable(k: &str) -> bool {
    key_ok(k) && !programs_own(k)
}

/// Where this person's desktop and what TorqOS last knew of it are.
pub struct Paths {
    pub cosmic: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn of_home() -> Result<Paths> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config"));
        Ok(Paths { cosmic: config.join("cosmic"), state: home.join(".local/state/torq/desktop-applied.json") })
    }
}

/// A folder's entries: its folders, and the names of those that are links.
fn dirs(p: &Path) -> (Vec<(String, PathBuf)>, Vec<String>) {
    let (mut real, mut links) = (vec![], vec![]);
    let Ok(entries) = std::fs::read_dir(p) else { return (real, links) };
    for e in entries.flatten() {
        let (Ok(t), Some(name)) = (e.file_type(), e.file_name().into_string().ok()) else { continue };
        if t.is_symlink() {
            links.push(name);
        } else if t.is_dir() {
            real.push((name, e.path()));
        }
    }
    (real, links)
}

/// This desktop's settings, as COSMIC wrote them, and what of it TorqOS leaves alone.
#[derive(Debug, Default, PartialEq)]
pub struct Local {
    pub values: BTreeMap<String, String>,
    /// Settings there but not kept: a link (a dotfile manager's), too large, past the limits. Neither saved,
    /// nor taken for a setting reset, nor written over.
    unmanaged: BTreeSet<String>,
    /// Folders that are links (`<component>/`, `<component>/v<n>/`): everything under them is someone else's.
    linked: Vec<String>,
}

impl Local {
    /// Whether TorqOS keeps setting `k` here: COSMIC's and the person's, in a real folder of theirs.
    pub fn manages(&self, k: &str) -> bool {
        !machine_specific(k) && !self.unmanaged.contains(k) && !self.linked.iter().any(|l| k.starts_with(l.as_str()))
    }
}

pub fn local(cosmic: &Path) -> Local {
    let mut out = Local::default();
    let mut found: Vec<(String, PathBuf)> = vec![];
    let (components, linked) = dirs(cosmic);
    out.linked.extend(linked.into_iter().map(|c| format!("{c}/")));
    for (component, c) in components {
        let (versions, linked) = dirs(&c);
        out.linked.extend(linked.into_iter().map(|v| format!("{component}/{v}/")));
        for (version, v) in versions {
            let Ok(files) = std::fs::read_dir(&v) else { continue };
            for f in files.flatten() {
                let Some(name) = f.file_name().into_string().ok() else { continue };
                let key = format!("{component}/{version}/{name}");
                if !keepable(&key) || machine_specific(&key) {
                    continue;
                }
                if f.file_type().is_ok_and(|t| t.is_file()) {
                    found.push((key, f.path()));
                } else {
                    out.unmanaged.insert(key);
                }
            }
        }
    }
    // In order of name, so that the same settings are kept each time, whatever order the folder lists them in.
    found.sort();
    let mut total = 0;
    for (key, path) in found {
        match std::fs::read_to_string(&path) {
            Ok(text) if value_ok(&text) && out.values.len() < MAX_KEYS && total + text.len() <= MAX_TOTAL => {
                total += text.len();
                out.values.insert(key, text);
            }
            _ => {
                out.unmanaged.insert(key);
            }
        }
    }
    out
}

/// What brings the desktop and the configuration together.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// The person's newer values, for the configuration (None: back to COSMIC's default).
    pub save: BTreeMap<String, Option<String>>,
    /// The configuration's newer values, for the desktop (None: COSMIC's default).
    pub apply: BTreeMap<String, Option<String>>,
}

/// `manages`: the settings TorqOS keeps here (`Local::manages`); the rest it neither saves nor writes, whatever
/// the configuration says (a pulled one's panel output, a value behind a dotfile manager's link).
pub fn plan(saved: &BTreeMap<String, String>, applied: &BTreeMap<String, String>, local: &BTreeMap<String, String>, manages: impl Fn(&str) -> bool) -> Plan {
    let mut p = Plan::default();
    let keys: BTreeSet<&String> = saved.keys().chain(applied.keys()).chain(local.keys()).filter(|k| manages(k)).collect();
    for k in keys {
        let (s, a, l) = (saved.get(k), applied.get(k), local.get(k));
        if l == a {
            // Not changed here since they agreed: the configuration's is newer, if it differs.
            if s != a {
                p.apply.insert(k.clone(), s.cloned());
            }
        } else if a.is_none() && s.is_some() {
            // Never agreed here and the configuration has it: a machine yet to take it (rebuilt, adopted).
            if l != s {
                p.apply.insert(k.clone(), s.cloned());
            }
        } else if s != l {
            p.save.insert(k.clone(), l.cloned());
        }
    }
    p
}

/// What the two agree on afterwards; where they still differ (a change not saved), what they last agreed.
fn agreed(saved: &BTreeMap<String, String>, local: &BTreeMap<String, String>, before: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let keys: BTreeSet<&String> = saved.keys().chain(local.keys()).chain(before.keys()).collect();
    keys.into_iter()
        .filter_map(|k| match (saved.get(k), local.get(k)) {
            (s, l) if s == l => s.map(|v| (k.clone(), v.clone())),
            _ => before.get(k).map(|v| (k.clone(), v.clone())),
        })
        .collect()
}

fn read_applied(state: &Path) -> BTreeMap<String, String> {
    std::fs::read(state).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_atomically(path: &Path, text: &[u8]) -> Result<()> {
    let dir = path.parent().context("a file has a folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.torq", path.file_name().and_then(|n| n.to_str()).unwrap_or("x")));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Put the configuration's values on the desktop: COSMIC sees its files change and takes them at once.
fn apply(cosmic: &Path, changes: &BTreeMap<String, Option<String>>) -> Vec<String> {
    let mut failed = vec![];
    for (k, v) in changes {
        // Checked again here: these names come from the configuration and become paths.
        if !key_ok(k) {
            failed.push(k.clone());
            continue;
        }
        let path = cosmic.join(k);
        // Never through a link: not the component's folder, its version's, or the file (a dotfile manager's).
        let (component, version) = (cosmic.join(k.split('/').next().unwrap_or_default()), path.parent().map(Path::to_path_buf).unwrap_or_default());
        if [&component, &version, &path].iter().any(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())) {
            failed.push(k.clone());
            continue;
        }
        let done = match v {
            Some(text) => write_atomically(&path, text.as_bytes()),
            None => match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                _ => Ok(()),
            },
        };
        if done.is_err() {
            failed.push(k.clone());
        }
    }
    failed
}

fn path_of(me: &str) -> String {
    format!("users/{me}/{FILE}")
}

/// The configuration's `desktop.json` of `me` at HEAD: its bytes, and what it says (an error, if it cannot be
/// read: written by hand, or by a newer TorqOS).
fn saved_of(repo: &Repo, me: &str) -> Result<(Option<Vec<u8>>, Result<Saved, String>)> {
    let bytes = repo.file_at("HEAD", &path_of(me))?;
    let saved = match &bytes {
        Some(b) => serde_json::from_slice(b).map_err(|e| format!("{} cannot be read ({e}): Save now replaces it with this desktop's settings", path_of(me))),
        None => Ok(Saved::default()),
    };
    Ok((bytes, saved))
}

/// A few words for what changed: "panel, theme".
pub fn what(keys: impl Iterator<Item = String>) -> String {
    let word = |k: &str| {
        let component = k.split('/').next().unwrap_or_default();
        // No longer kept, and only ever named as taken out: another program's, or a COSMIC program's own state.
        if !component.starts_with(COSMIC) {
            return "other programs' settings".to_string();
        }
        if programs_own(k) {
            return "the screenshot tool's last area".to_string();
        }
        let c = component.strip_prefix(COSMIC).unwrap_or(component);
        let c = c.strip_prefix("Cosmic").unwrap_or(c);
        match c.split('.').next().unwrap_or(c) {
            "" => component.to_string(),
            "Theme" | "Tk" => "theme".to_string(),
            "Background" => "wallpaper".to_string(),
            "Panel" if c.ends_with("Dock") => "dock".to_string(),
            "Panel" => "panel".to_string(),
            "AppList" => "dock's apps".to_string(),
            "Settings" if c.ends_with("Shortcuts") => "shortcuts".to_string(),
            "Comp" => "windows and input".to_string(),
            // An applet by its name ("AppletAudio": audio); one named only "Applet", by that.
            other => match other.trim_start_matches("Applet") {
                "" => other.to_lowercase(),
                name => name.to_lowercase(),
            },
        }
    };
    let words: BTreeSet<String> = keys.map(|k| word(&k)).collect();
    let mut list: Vec<String> = words.into_iter().collect();
    if list.len() > 4 {
        let n = list.len() - 3;
        list.truncate(3);
        list.push(format!("{n} more"));
    }
    list.join(", ")
}

/// A save's label: what the person changed, and what an earlier TorqOS kept that is taken out, both named.
fn label(changed: &[String], gone: &[String]) -> String {
    let out = || format!("{} taken out (only COSMIC's own settings are kept)", what(gone.iter().cloned()));
    match (changed.is_empty(), gone.is_empty()) {
        (_, true) => format!("Desktop: {}", what(changed.iter().cloned())),
        (true, false) => format!("Desktop: {}", out()),
        (false, false) => format!("Desktop: {}; {}", what(changed.iter().cloned()), out()),
    }
}

/// What of a saved file is no longer kept: another program's settings, a COSMIC program's own state.
fn not_kept(saved: &Saved) -> Vec<String> {
    saved.cosmic.keys().filter(|k| !keepable(k)).cloned().collect()
}

/// Commit `me`'s new `desktop.json`: their own folder, no question.
fn commit(machine: &dyn Machine, repo: &Repo, me: &str, old: Option<&[u8]>, new: &Saved, reason: &str) -> Result<u64> {
    check(new)?;
    let scratch = crate::scratch_dir()?;
    let text = new.render()?;
    if text.len() > 4 * 1024 * 1024 {
        bail!("too much to save: {} bytes", text.len());
    }
    let diff = crate::candidate::unified_diff_bytes(scratch.path(), &[(path_of(me), old, Some(text.as_bytes()))])?;
    machine.commit(&diff, &repo.head()?, "user", reason, None)
}

/// How a sync went.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Synced {
    /// The change that saved the person's settings.
    pub saved: Option<u64>,
    /// Settings the configuration put on the desktop.
    pub applied: usize,
    /// The person's changes not saved (saving is manual).
    pub unsaved: usize,
    /// Settings that could not be put on the desktop.
    pub failed: Vec<String>,
}

/// Bring the desktop and the configuration together: the configuration's newer values onto the desktop,
/// and the person's saved when saving is automatic or `save_now`.
pub fn sync(machine: &dyn Machine, repo: &Repo, me: &str, paths: &Paths, save_now: bool) -> Result<Synced> {
    sync_by(repo, me, paths, save_now, &mut |old, new, reason| commit(machine, repo, me, old, new, reason))
}

/// `sync`, with how a save is committed given: the broker, or a test's stand-in.
fn sync_by(repo: &Repo, me: &str, paths: &Paths, save_now: bool, save: &mut dyn FnMut(Option<&[u8]>, &Saved, &str) -> Result<u64>) -> Result<Synced> {
    let (bytes, read) = saved_of(repo, me)?;
    // A file that cannot be read is replaced only when the person asks (Save now), by this desktop's settings.
    let (mut saved, mut before, repairing) = match read {
        Ok(s) => (s, read_applied(&paths.state), false),
        Err(_) if save_now => {
            (Saved::default(), BTreeMap::new(), true)
        }
        Err(why) => bail!("{why}"),
    };
    // Saved by an earlier TorqOS, and no longer kept (another program's settings, a program's own state): taken
    // out of the saved file at the next save, and never put back on the desktop.
    let dropped = not_kept(&saved);
    saved.cosmic.retain(|k, _| keepable(k));
    before.retain(|k, _| keepable(k));
    let here = local(&paths.cosmic);
    let p = plan(&saved.cosmic, &before, &here.values, |k| here.manages(k));
    let mut out = Synced { failed: apply(&paths.cosmic, &p.apply), ..Synced::default() };
    out.applied = p.apply.len() - out.failed.len();
    let mut values = here.values.clone();
    for (k, v) in &p.apply {
        if out.failed.contains(k) {
            continue;
        }
        match v {
            Some(v) => values.insert(k.clone(), v.clone()),
            None => values.remove(k),
        };
    }
    let mut now = saved.clone();
    if (!p.save.is_empty() || repairing || !dropped.is_empty()) && (save_now || saved.save == Saving::Automatic) {
        for (k, v) in &p.save {
            match v {
                Some(v) => now.cosmic.insert(k.clone(), v.clone()),
                None => now.cosmic.remove(k),
            };
        }
        let reason = if repairing {
            "Desktop: saved again (the saved file could not be read)".to_string()
        } else {
            label(&p.save.keys().cloned().collect::<Vec<_>>(), &dropped)
        };
        out.saved = Some(save(bytes.as_deref(), &now, &reason)?);
    } else {
        out.unsaved = p.save.len();
    }
    let state = agreed(&now.cosmic, &values, &before);
    write_atomically(&paths.state, &serde_json::to_vec(&state)?)?;
    Ok(out)
}

/// Save automatically, or only when asked.
pub fn set_saving(machine: &dyn Machine, repo: &Repo, me: &str, how: Saving) -> Result<String> {
    set_saving_by(repo, me, how, &mut |old, new, reason| commit(machine, repo, me, old, new, reason))
}

/// `set_saving`, with how the change is committed given (as `sync_by`).
fn set_saving_by(repo: &Repo, me: &str, how: Saving, save: &mut dyn FnMut(Option<&[u8]>, &Saved, &str) -> Result<u64>) -> Result<String> {
    let (bytes, read) = saved_of(repo, me)?;
    let mut saved = read.map_err(anyhow::Error::msg)?;
    if saved.save == how {
        return Ok(format!("Your desktop is already saved {}.\n", words(how)));
    }
    // What the broker no longer takes leaves with this change (it would refuse the file otherwise), and the label
    // says so.
    let gone = not_kept(&saved);
    saved.cosmic.retain(|k, _| keepable(k));
    saved.save = how;
    let reason = match gone.is_empty() {
        true => format!("Desktop: saved {}", words(how)),
        false => format!("Desktop: saved {}; {} taken out (only COSMIC's own settings are kept)", words(how), what(gone.into_iter())),
    };
    let seq = save(bytes.as_deref(), &saved, &reason)?;
    Ok(format!("Your desktop is saved {} from now on (change #{seq}).\n", words(how)))
}

fn words(how: Saving) -> &'static str {
    match how {
        Saving::Automatic => "automatically",
        Saving::Manual => "only when you ask",
    }
}

/// Where the desktop and the configuration stand, for `show system desktop` and TorqOS Settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Status {
    pub saving: Saving,
    pub saved: usize,
    /// The person's changes not yet saved, by a few words.
    pub unsaved: Vec<String>,
    /// The configuration's values still to be put on the desktop.
    pub to_apply: usize,
    /// What an earlier TorqOS saved that is no longer kept (another program's settings): taken out by the next save.
    pub to_take_out: Vec<String>,
}

pub fn status(repo: &Repo, me: &str, paths: &Paths) -> Result<Status> {
    let (_, read) = saved_of(repo, me)?;
    let mut saved = read.map_err(anyhow::Error::msg)?;
    let to_take_out = not_kept(&saved);
    saved.cosmic.retain(|k, _| keepable(k));
    let mut applied = read_applied(&paths.state);
    applied.retain(|k, _| keepable(k));
    let here = local(&paths.cosmic);
    let p = plan(&saved.cosmic, &applied, &here.values, |k| here.manages(k));
    Ok(Status { saving: saved.save, saved: saved.cosmic.len(), unsaved: p.save.keys().cloned().collect(), to_apply: p.apply.len(), to_take_out })
}

/// "1 setting", "3 settings".
fn settings(n: usize) -> String {
    if n == 1 { "1 setting".to_string() } else { format!("{n} settings") }
}

/// For `show system desktop` and TorqOS Settings: what the next save takes out, in a sentence.
pub fn taking_out(keys: &[String]) -> Option<String> {
    (!keys.is_empty()).then(|| {
        format!(
            "Saved by an earlier TorqOS and no longer kept: {} ({}). The next save takes them out of your configuration; they stay on this desktop, and in the history.",
            what(keys.iter().cloned()),
            settings(keys.len())
        )
    })
}

pub fn shown(s: &Status) -> String {
    let mut out = format!("Your desktop (COSMIC's settings): {} saved in your configuration, saved {}.\n", s.saved, words(s.saving));
    if s.unsaved.is_empty() {
        out.push_str("  Everything you changed is saved.\n");
    } else {
        out.push_str(&format!("  Not saved yet: {} ({}). request system desktop save saves them.\n", what(s.unsaved.iter().cloned()), settings(s.unsaved.len())));
    }
    if let Some(t) = taking_out(&s.to_take_out) {
        out.push_str(&format!("  {t} request system desktop save does it now.\n"));
    }
    if s.to_apply > 0 {
        out.push_str(&format!("  {} settings from your configuration are still to be put on this desktop (at the next login, or request system desktop save).\n", s.to_apply));
    }
    out
}

/// Refuse a saved file the broker would refuse: said here first, in these words.
pub fn check(saved: &Saved) -> Result<()> {
    let total: usize = saved.cosmic.values().map(String::len).sum();
    if saved.cosmic.len() > MAX_KEYS || total > MAX_TOTAL {
        bail!("too much to save: {} settings, {} bytes", saved.cosmic.len(), total);
    }
    if let Some((k, _)) = saved.cosmic.iter().find(|(k, v)| !key_ok(k) || !value_ok(v)) {
        bail!("{k} is not a setting TorqOS can save");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }
    const K: &str = "com.system76.CosmicPanel.Panel/v1/size";
    fn plan3(s: &BTreeMap<String, String>, a: &BTreeMap<String, String>, l: &BTreeMap<String, String>) -> Plan {
        plan(s, a, l, |_| true)
    }

    #[test]
    fn a_settings_name_is_cosmics_layout_and_never_a_path() {
        assert!(key_ok(K));
        assert!(key_ok("com.system76.CosmicBackground/v1/same-on-all"));
        for bad in ["", "a/v1/b", "com.x/1/b", "com.x/v1", "com.x/v1/b/c", "../x/v1/b", "com.x/v1/..", "com..x/v1/b", "com.x/v1/-b", "com.x/v12345/b", "com.x/v1/a b"] {
            assert!(!key_ok(bad), "{bad}");
        }
        assert!(machine_specific("com.system76.CosmicPanel.Dock/v1/output"));
        assert!(machine_specific("com.system76.CosmicIdle/v1/suspend_on_ac_time"));
        assert!(!machine_specific("com.system76.CosmicIdle/v1/screen_off_time"));
        assert!(!machine_specific(K));
    }

    #[test]
    fn the_newer_side_wins() {
        let none = map(&[]);
        // The person changed it: saved.
        let p = plan3(&map(&[(K, "S")]), &map(&[(K, "S")]), &map(&[(K, "L")]));
        assert_eq!(p.save, [(K.to_string(), Some("L".to_string()))].into());
        assert!(p.apply.is_empty());
        // The configuration changed (an undo, a pull): onto the desktop.
        let p = plan3(&map(&[(K, "S2")]), &map(&[(K, "S")]), &map(&[(K, "S")]));
        assert_eq!(p.apply, [(K.to_string(), Some("S2".to_string()))].into());
        // An undo of a first save: back to COSMIC's default.
        let p = plan3(&none, &map(&[(K, "L")]), &map(&[(K, "L")]));
        assert_eq!(p.apply, [(K.to_string(), None)].into());
        // Reset to default here: no longer kept.
        let p = plan3(&map(&[(K, "S")]), &map(&[(K, "S")]), &none);
        assert_eq!(p.save, [(K.to_string(), None)].into());
        // A rebuilt machine: COSMIC's defaults there give way to the configuration.
        let p = plan3(&map(&[(K, "S")]), &none, &map(&[(K, "default")]));
        assert_eq!(p.apply, [(K.to_string(), Some("S".to_string()))].into());
        // The first save on a machine: everything here.
        let p = plan3(&none, &none, &map(&[(K, "L")]));
        assert_eq!(p.save, [(K.to_string(), Some("L".to_string()))].into());
        // In step: nothing.
        assert_eq!(plan3(&map(&[(K, "S")]), &map(&[(K, "S")]), &map(&[(K, "S")])), Plan::default());
    }

    #[test]
    fn a_change_not_saved_stays_one() {
        // Manual: the person's L is not saved; what they agreed stays S, so L is still to save next time.
        let state = agreed(&map(&[(K, "S")]), &map(&[(K, "L")]), &map(&[(K, "S")]));
        assert_eq!(state, map(&[(K, "S")]));
        assert_eq!(plan3(&map(&[(K, "S")]), &state, &map(&[(K, "L")])).save.len(), 1);
        // Saved: they agree on L.
        assert_eq!(agreed(&map(&[(K, "L")]), &map(&[(K, "L")]), &map(&[(K, "S")])), map(&[(K, "L")]));
    }

    #[test]
    fn the_desktop_is_read_as_cosmic_lays_it_out_and_written_back() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("cosmic");
        let w = |k: &str, v: &str| write_atomically(&c.join(k), v.as_bytes()).unwrap();
        w(K, "M");
        w("com.system76.CosmicPanel.Panel/v1/output", "\"eDP-1\"");
        w("com.system76.CosmicPanel./v1/x", "odd");
        std::os::unix::fs::symlink("/etc/passwd", c.join("com.system76.CosmicPanel.Panel/v1/link")).unwrap();
        assert_eq!(local(&c).values, map(&[(K, "M")]), "machine-specific, odd names and links left out");
        let failed = apply(&c, &[(K.to_string(), Some("L".to_string())), ("../../evil/v1/x".to_string(), Some("x".to_string()))].into());
        assert_eq!(failed, ["../../evil/v1/x"]);
        assert_eq!(std::fs::read_to_string(c.join(K)).unwrap(), "L");
        assert!(!d.path().join("evil").exists());
        apply(&c, &[(K.to_string(), None)].into());
        assert!(!c.join(K).exists());
    }


    #[test]
    fn what_is_behind_a_link_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let (c, elsewhere) = (d.path().join("cosmic"), d.path().join("dotfiles"));
        std::fs::create_dir_all(elsewhere.join("v1")).unwrap();
        std::fs::write(elsewhere.join("v1/accent"), "red").unwrap();
        std::fs::create_dir_all(&c).unwrap();
        std::os::unix::fs::symlink(&elsewhere, c.join("com.system76.CosmicTheme.Dark")).unwrap();
        write_atomically(&c.join("com.system76.CosmicTk/v1/x"), b"real").unwrap();
        std::os::unix::fs::symlink(elsewhere.join("v1/accent"), c.join("com.system76.CosmicTk/v1/linked")).unwrap();
        let here = local(&c);
        let dark = "com.system76.CosmicTheme.Dark/v1/accent";
        assert!(!here.manages(dark) && !here.manages("com.system76.CosmicTk/v1/linked") && here.manages("com.system76.CosmicTk/v1/x"));
        // Saved before, behind a link now: not taken for a reset, and not written through the link.
        let saved = map(&[(dark, "blue")]);
        let p = plan(&saved, &saved, &here.values, |k| here.manages(k));
        assert!(!p.save.contains_key(dark) && p.apply.is_empty(), "{p:?}");
        assert_eq!(apply(&c, &[(dark.to_string(), Some("blue".to_string()))].into()), [dark]);
        assert_eq!(std::fs::read_to_string(elsewhere.join("v1/accent")).unwrap(), "red");
        // A pulled configuration's panel output is this machine's to keep.
        let out = "com.system76.CosmicPanel.Panel/v1/output";
        assert!(plan(&map(&[(out, "\"HDMI-1\"")]), &map(&[]), &map(&[]), |k| here.manages(k)).apply.is_empty());
    }
    #[test]
    fn the_saved_file_reads_back_and_says_what_changed() {
        let s = Saved { save: Saving::Manual, cosmic: map(&[(K, "M")]) };
        assert_eq!(serde_json::from_str::<Saved>(&s.render().unwrap()).unwrap(), s);
        assert_eq!(serde_json::from_str::<Saved>("{}").unwrap(), Saved::default());
        assert!(serde_json::from_str::<Saved>(r#"{"other": 1}"#).is_err());
        assert_eq!(what([K.to_string(), "com.system76.CosmicTheme.Dark/v1/accent".to_string()].into_iter()), "panel, theme");
        assert!(check(&Saved { cosmic: map(&[("x/v1/y", "z")]), ..Saved::default() }).is_err());
    }

    const TEMPEST: &str = "com.vintagetechie.CosmicExtAppletTempest/v1/latitude";
    const SHOT: &str = "com.system76.CosmicPortal/v1/screenshot";

    /// Only COSMIC's own settings are kept: another program's (a weather applet's location) is never read off
    /// the desktop, never sent, and named for what it is.
    #[test]
    fn another_programs_settings_are_never_kept() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("cosmic");
        write_atomically(&c.join(K), b"M").unwrap();
        write_atomically(&c.join(TEMPEST), b"45.1").unwrap();
        write_atomically(&c.join(SHOT), b"(last_rectangle: None)").unwrap();
        assert_eq!(local(&c).values, map(&[(K, "M")]), "another program's settings and a program's own state left out");
        assert!(!key_ok(TEMPEST) && !key_ok("com.system76/v1/x") && !key_ok("org.system76.CosmicPanel/v1/x"));
        assert!(check(&Saved { cosmic: map(&[(K, "M"), (TEMPEST, "45.1")]), ..Saved::default() }).is_err());
        assert_eq!(what([TEMPEST.to_string()].into_iter()), "other programs' settings");
        assert_eq!(what(["com.system76.CosmicAppletAudio/v1/x".to_string(), "com.system76.Foo/v1/x".to_string()].into_iter()), "audio, foo");
        // Never put on a desktop from a configuration either.
        assert_eq!(apply(&c, &[(TEMPEST.to_string(), Some("0".to_string()))].into()), [TEMPEST]);
    }

    fn sh(dir: &Path, script: &str) {
        let st = std::process::Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }

    /// A configuration repository holding `old` as `users/dev/desktop.json`, and a desktop's paths beside it.
    fn configured(old: &Saved) -> (tempfile::TempDir, PathBuf, Paths) {
        let d = tempfile::tempdir().unwrap();
        let (root, home) = (d.path().join("config"), d.path().join("home"));
        std::fs::create_dir_all(root.join("users/dev")).unwrap();
        std::fs::write(root.join("users/dev/desktop.json"), old.render().unwrap()).unwrap();
        sh(&root, "git init -q -b main && git config user.name t && git config user.email t@t && git add -A && git commit -qm one");
        let paths = Paths { cosmic: home.join("cosmic"), state: home.join("state/desktop-applied.json") };
        (d, root, paths)
    }

    /// Saving manual, a desktop.json from an earlier TorqOS: nothing is saved by a sync; what will be taken out is
    /// said as that, not as "not saved yet"; turning saving on takes it out, and the label says so.
    #[test]
    fn what_an_earlier_torqos_kept_is_said_as_taken_out() {
        let old = Saved { save: Saving::Manual, cosmic: map(&[(K, "M"), (TEMPEST, "45.1")]) };
        let (_d, root, paths) = configured(&old);
        write_atomically(&paths.cosmic.join(K), b"M").unwrap();
        let repo = Repo::new(&root);
        let mut never = |_: Option<&[u8]>, _: &Saved, r: &str| -> Result<u64> { panic!("saved while manual: {r}") };
        sync_by(&repo, "dev", &paths, false, &mut never).unwrap();
        let s = status(&repo, "dev", &paths).unwrap();
        assert_eq!((s.unsaved.len(), s.to_take_out.clone(), s.saved), (0, vec![TEMPEST.to_string()], 1), "{s:?}");
        let text = shown(&s);
        assert!(text.contains("Everything you changed is saved.") && text.contains("no longer kept: other programs' settings (1 setting)"), "{text}");
        assert!(!text.contains("Not saved yet"), "{text}");
        let mut got = vec![];
        let mut save = |_: Option<&[u8]>, new: &Saved, r: &str| -> Result<u64> {
            check(new)?;
            got.push((r.to_string(), new.clone()));
            Ok(1)
        };
        set_saving_by(&repo, "dev", Saving::Automatic, &mut save).unwrap();
        let (reason, new) = got.pop().unwrap();
        assert_eq!(reason, "Desktop: saved automatically; other programs' settings taken out (only COSMIC's own settings are kept)");
        assert_eq!((new.save, new.cosmic), (Saving::Automatic, map(&[(K, "M")])));
    }

    /// Each save's label is true: an applet named only "Applet", a cleanup of COSMIC's own state, a cleanup with a
    /// change of the person's.
    #[test]
    fn a_saves_label_names_what_it_does() {
        let s = |v: &[&str]| v.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(what(["com.system76.CosmicApplet/v1/x".to_string()].into_iter()), "applet");
        assert_eq!(label(&[], &s(&[SHOT])), "Desktop: the screenshot tool's last area taken out (only COSMIC's own settings are kept)");
        assert_eq!(label(&s(&[K]), &s(&[TEMPEST])), "Desktop: panel; other programs' settings taken out (only COSMIC's own settings are kept)");
        assert_eq!(label(&s(&[K]), &[]), "Desktop: panel");
    }

    /// The configuration repository with `desktop.json` as an earlier TorqOS saved it, and a desktop: a sync saves
    /// once to take the other program's settings out, then nothing until the person changes something, whatever
    /// the other program and COSMIC's screenshot tool rewrite meanwhile.
    #[test]
    fn a_sync_with_nothing_the_person_changed_saves_nothing() {
        let d = tempfile::tempdir().unwrap();
        let (root, home) = (d.path().join("config"), d.path().join("home"));
        std::fs::create_dir_all(root.join("users/dev")).unwrap();
        let old = Saved { save: Saving::Automatic, cosmic: map(&[(K, "M"), (TEMPEST, "45.1")]) };
        std::fs::write(root.join("users/dev/desktop.json"), old.render().unwrap()).unwrap();
        sh(&root, "git init -q -b main && git config user.name t && git config user.email t@t && git add -A && git commit -qm one");
        let paths = Paths { cosmic: home.join("cosmic"), state: home.join("state/desktop-applied.json") };
        let w = |k: &str, v: &str| write_atomically(&paths.cosmic.join(k), v.as_bytes()).unwrap();
        w(K, "M");
        w(TEMPEST, "45.1");
        w(SHOT, "a");
        // Each save the broker would take: written and committed, as it would.
        let repo = Repo::new(&root);
        let saves = std::cell::RefCell::new(vec![]);
        let mut save = |_: Option<&[u8]>, new: &Saved, reason: &str| -> Result<u64> {
            check(new)?;
            std::fs::write(root.join("users/dev/desktop.json"), new.render()?)?;
            sh(&root, "git commit -qam save");
            saves.borrow_mut().push((reason.to_string(), new.clone()));
            Ok(saves.borrow().len() as u64)
        };
        let s = sync_by(&repo, "dev", &paths, false, &mut save).unwrap();
        assert!(s.saved.is_some() && s.failed.is_empty(), "{s:?}");
        let (reason, new) = saves.borrow()[0].clone();
        assert_eq!(new.cosmic, map(&[(K, "M")]), "what an earlier TorqOS saved of another program's is taken out");
        assert!(reason.contains("other programs' settings taken out"), "{reason}");
        // The applet refreshes its weather, a screenshot is taken: nothing to save.
        w(TEMPEST, "45.2");
        w(SHOT, "b");
        for _ in 0..2 {
            assert_eq!(sync_by(&repo, "dev", &paths, false, &mut save).unwrap(), Synced::default());
        }
        assert_eq!(saves.borrow().len(), 1);
        // The person changes the panel: saved, and named.
        w(K, "L");
        sync_by(&repo, "dev", &paths, false, &mut save).unwrap();
        assert_eq!(saves.borrow().last().unwrap().0, "Desktop: panel");
        assert_eq!(saves.borrow().len(), 2);
    }
}
