//! The whole configuration (docs/CONFIG-IS-THE-DEVICE.md, S3c): one tree of NixOS paths over the
//! config repo's two kinds of data files. Most settings live in `nixos/<top-level>.json`
//! (settings.rs), checked against the system's own NixOS settings (nixos.rs). A few keep the files
//! TorqOS gave them before (tree.rs and the hand-made schema), for their own checks and tiers — above
//! a person's own packages, which are user tier: changed without a question.
//!
//! `MAP` says which, and what each was called before: those names still work (`set system host-name
//! x`), the tree always shows the NixOS one (`networking hostName`).

use crate::nixos::Tree;
use crate::settings::{self, Settings};
use crate::tree::{Config, Kind, Schema, Val};
use crate::attach::{self, Attached};
use crate::software;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

/// (NixOS path, the hand-made schema's path to the file that holds it, whether the old name still
/// works as an alias). `<name>` is a list entry on both sides.
pub const MAP: &[(&str, &str, bool)] = &[
    ("networking hostName", "system host-name", true),
    ("time timeZone", "system time-zone", true),
    // Software: NixOS's own places, kept in TorqOS's files (a person's own: user tier).
    ("environment systemPackages", "system packages", true),
    ("users users <name> packages", "users <name> packages", true),
    // The old list of services (services.json): shown, and emptied, never added to (OLD_SERVICES).
    ("torq services enabled", "services", false),
];

/// The old list's names (services.json, before ADR-025), and the NixOS setting each is now
/// (modules/services.nix maps the old list the same way).
pub const OLD_SERVICES: &[(&str, &str)] = &[
    ("ssh", "services openssh enable true"),
    ("podman", "virtualisation podman enable true"),
    ("tailscale", "services tailscale enable true"),
    ("printing", "services printing enable true"),
];

/// An error of the hand-made schema about `hand`, in the words the person typed (`typed`): the
/// setting named as they named it (`networking hostName`, not the file's `system host-name`).
fn as_typed(e: anyhow::Error, typed: &[String], hand: &[String]) -> anyhow::Error {
    let said = format!("{e:#}");
    for (nixos, h, _) in MAP {
        let (n, hw): (Vec<&str>, Vec<&str>) = (nixos.split(' ').collect(), h.split(' ').collect());
        let fits = |words: &[String], pat: &[&str]| words.len() >= pat.len() && pat.iter().zip(words).all(|(p, w)| *p == "<name>" || p == w);
        if fits(typed, &n) && fits(hand, &hw) {
            return anyhow::anyhow!(said.replace(&hand[..hw.len()].join(" "), &typed[..n.len()].join(" ")));
        }
    }
    e
}

/// The short names (MAP's old names that still work) that may be typed next after `path`: each one's
/// next word, its hand-made schema node (for its help), and the setting it is short for, as `?`
/// offers them. `is_place(words)`: whether the tree has a place or setting there — a `<name>` never
/// stands for one of the tree's own words (`users users` is NixOS's, not a person), and a short name
/// never hides a real one.
pub fn shorthands_after(path: &[String], is_place: impl Fn(&[String]) -> bool) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (nixos, hand, alias) in MAP {
        let pat: Vec<&str> = hand.split(' ').collect();
        if !alias || pat.len() != path.len() + 1 {
            continue;
        }
        let mut name = None;
        let fits = pat.iter().zip(path).enumerate().all(|(i, (p, w))| match *p {
            "<name>" => {
                name = Some(w.clone());
                !is_place(&[&path[..i], std::slice::from_ref(w)].concat())
            }
            p => p == w,
        });
        let next = pat[path.len()].to_string();
        if !fits || is_place(&[path.to_vec(), vec![next.clone()]].concat()) {
            continue;
        }
        let real = nixos.split(' ').map(|p| if p == "<name>" { name.clone().unwrap_or_default() } else { p.to_string() }).collect::<Vec<_>>().join(" ");
        out.push((next, hand.to_string(), real));
    }
    out
}

/// `words` with a prefix matching one side of MAP (0: NixOS, 1: hand-made) replaced by the other.
/// Words as the hand-made schema names them, if they name a setting TorqOS keeps in its own files:
/// by its NixOS path (`environment systemPackages`), or by its old name (`system packages`), as `set`
/// reads them.
pub fn hand_words(words: &[String]) -> Option<Vec<String>> {
    translate(words, 0, false).or_else(|| translate(words, 1, true).map(|_| words.to_vec()))
}

fn translate(words: &[String], from: usize, aliases_only: bool) -> Option<Vec<String>> {
    for (nixos, hand, alias) in MAP {
        if aliases_only && !alias {
            continue;
        }
        let (a, b): (Vec<&str>, Vec<&str>) = if from == 0 {
            (nixos.split(' ').collect(), hand.split(' ').collect())
        } else {
            (hand.split(' ').collect(), nixos.split(' ').collect())
        };
        if words.len() < a.len() {
            continue;
        }
        let mut name = None;
        let ok = a.iter().zip(words).all(|(p, w)| {
            if *p == "<name>" {
                name = Some(w.clone());
                true
            } else {
                p == w
            }
        });
        if ok {
            let mut out: Vec<String> = b.iter().map(|p| if *p == "<name>" { name.clone().unwrap_or_default() } else { p.to_string() }).collect();
            out.extend_from_slice(&words[a.len()..]);
            return Some(out);
        }
    }
    None
}

/// The whole tree, given the hand-made schema and the NixOS settings tree.
pub struct Whole<'a> {
    pub schema: &'a Schema,
    pub tree: &'a Tree,
}

/// A folder (`path:`) as where TorqOS comes from is for developing TorqOS: root builds from it, so
/// only with `torq dev enable true` (second audit, M3). Checked at `set` and at `commit`.
pub fn software_allowed(d: &Data) -> Result<()> {
    let dev = d.settings.get("torq").and_then(|t| t.pointer("/dev/enable")).and_then(serde_json::Value::as_bool) == Some(true);
    if d.source.as_deref().is_some_and(software::writable_folder) && !dev {
        bail!("TorqOS from a folder (path:) is for developing TorqOS, with `torq dev enable true`: root builds from it");
    }
    Ok(())
}

/// `system software torq source …`: the words after the path (None: not that setting). Only `torq`
/// has a source of its own (nixpkgs follows TorqOS's).
fn software_words(words: &[String]) -> Option<&[String]> {
    let n = software::PATH.len();
    (words.len() >= n && words[..n].iter().zip(software::PATH).all(|(a, b)| a == b)).then(|| &words[n..])
}

/// What a `delete` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deleted {
    Done,
    /// Nothing named is set: nothing deleted (a warning, not an error).
    NotFound,
}

/// The tree's data: the files TorqOS keeps (the hand-made schema's), nixos/*.json, the config files
/// attached (config-files.json), and the software's source (flake.nix).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Data {
    pub config: Config,
    pub settings: Settings,
    pub attached: Attached,
    /// Where TorqOS comes from (`system software torq source`, software.rs); None: flake.nix is not
    /// TorqOS's fixed one (or there is none).
    pub source: Option<String>,
}

impl Whole<'_> {
    /// Words as the hand-made schema's, if they name a file-backed setting: by its NixOS path, or by
    /// its old name. Words that match an old name are the hand-made schema's to decide: its error, if
    /// it refuses them.
    fn hand(&self, words: &[String], cfg: &Config) -> Result<Option<Vec<String>>> {
        if let Some(h) = translate(words, 0, false) {
            return Ok(Some(h));
        }
        if translate(words, 1, true).is_none() {
            return Ok(None);
        }
        self.schema.resolve_in(words, Some(cfg))?;
        Ok(Some(words.to_vec()))
    }

    /// `<place> config [<file>]`: the place a config file is (to be) attached at, and the file. None if
    /// the words are not that — and where NixOS has a setting named `config` (`containers <name>
    /// config`), they are NixOS's.
    fn attachment<'w>(&self, words: &'w [String]) -> Option<(&'w [String], Option<Vec<String>>)> {
        let i = words.iter().rposition(|w| w == "config")?;
        let (place, rest) = (&words[..i], &words[i + 1..]);
        // Files: none named (None: all of a place's), one, or `[ a b ]` (as a saved configuration
        // writes several; `[ ]` names none, which is no file at all).
        let files = match rest {
            [] => None,
            [one] if one != "[" && one != "]" => Some(vec![one.clone()]),
            [open, inner @ .., close] if open == "[" && close == "]" => Some(inner.to_vec()),
            _ => return None,
        };
        // Below a set of named entries (`virtualHosts`), `config` is an entry's name.
        if self.tree.next(place).iter().any(|(w, _)| w == "config" || w.starts_with('<')) {
            return None;
        }
        let somewhere = place.is_empty() || self.is_place(place) || self.tree.locate(place).is_ok();
        somewhere.then_some((place, files))
    }

    /// Every value, by NixOS path, sorted: the files TorqOS kept, nixos/*.json, the config files.
    pub fn entries(&self, d: &Data) -> Vec<(Vec<String>, Val)> {
        let mut all: Vec<(Vec<String>, Val)> = self
            .schema
            .entries(&d.config)
            .into_iter()
            .map(|(w, v)| (translate(&w, 1, false).unwrap_or(w), v))
            .collect();
        all.extend(settings::entries(&d.settings));
        all.extend(attach::entries(&d.attached));
        if let Some(s) = &d.source {
            all.push((software::PATH.map(String::from).to_vec(), Val::One(s.clone())));
        }
        all.sort_by(|a, b| a.0.cmp(&b.0));
        all
    }

    /// The config repo file each entry comes from, by NixOS path.
    pub fn files(&self, d: &Data) -> BTreeMap<Vec<String>, String> {
        let mut out = BTreeMap::new();
        for (file, held) in self.schema.files_of(&d.config, &[]) {
            for h in held {
                let w: Vec<String> = h.split(' ').map(String::from).collect();
                out.insert(translate(&w, 1, false).unwrap_or(w), file.clone());
            }
        }
        for (w, _) in settings::entries(&d.settings) {
            let file = format!("{}{}.json", settings::DIR, w[0]);
            out.insert(w, file);
        }
        for (w, _) in attach::entries(&d.attached) {
            out.insert(w, attach::FILE.to_string());
        }
        if d.source.is_some() {
            out.insert(software::PATH.map(String::from).to_vec(), "flake.nix".into());
        }
        out
    }

    /// `set <words>` (NixOS path, or an old name; `<place> config <file>` attaches a config file).
    /// The hand-made schema of the configuration `d` is: its people are the machine's, and anyone
    /// the configuration declares (`users users <name>`), so a person added in the candidate can be
    /// given software in the same one.
    fn of(&self, d: &Data) -> Option<Schema> {
        let declared: Vec<String> = d.settings.get("users").and_then(|u| u.get("users")).and_then(|u| u.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default();
        let list = self.schema.nodes.get("users")?;
        let known = list.values.as_ref()?;
        let new: Vec<&String> = declared.iter().filter(|n| !known.contains_key(*n)).collect();
        if new.is_empty() {
            return None;
        }
        let mut s = self.schema.clone();
        let values = s.nodes.get_mut("users")?.values.as_mut()?;
        for n in new {
            values.insert(n.clone(), String::new());
        }
        Some(s)
    }

    pub fn set(&self, d: &mut Data, words: &[String]) -> Result<()> {
        match self.of(d) {
            Some(schema) => Whole { schema: &schema, tree: self.tree }.set_in(d, words),
            None => self.set_in(d, words),
        }
    }

    /// `words` as the tree names them (abbreviations written out, `networkin hostName` →
    /// `networking hostName`), so where a setting is kept is decided on its name, never on how it was
    /// typed; words the tree does not take (an old name: `system host-name`) as they are.
    fn spelled(&self, words: &[String]) -> Vec<String> {
        match self.tree.locate(words) {
            Ok((loc, _, values)) => [loc, values].concat(),
            Err(_) => words.to_vec(),
        }
    }

    fn set_in(&self, d: &mut Data, words: &[String]) -> Result<()> {
        let words: &[String] = &self.spelled(words);
        if let Some(rest) = software_words(words) {
            return match rest {
                [url] => {
                    if d.source.is_none() {
                        bail!("this machine's flake.nix is not TorqOS's fixed one: its software source cannot be set here");
                    }
                    software::valid(url)?;
                    d.source = Some(url.clone());
                    // (Whether this machine may build from it — software_allowed — is a rule between
                    // settings: checked on the whole configuration, as `commit` does, not line by line.)
                    Ok(())
                }
                _ => bail!("set system software torq source <source>: one source (like github:netstacks/torqos); the version is `request system update`'s"),
            };
        }
        // A config file joins the place's files, as a value joins a list (`set config motd.nix` does not
        // take hardware-configuration.nix's place).
        if let Some((place, files)) = self.attachment(words) {
            let files = files.filter(|f| !f.is_empty()).context("config <file>: which file (a .nix file of the config repo, like services/httpd.nix)")?;
            for f in &files {
                attach::valid(f)?;
            }
            d.attached.entry(place.join(" ")).or_default().extend(files);
            return Ok(());
        }
        if let Some(h) = self.hand(words, &d.config)? {
            if let ["services", name, ..] = h.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                let now = OLD_SERVICES.iter().find(|(old, _)| old == name).map_or("services <name> enable true", |(_, new)| new);
                bail!("torq services enabled is the old list of services: `set {now}` turns it on (TorqOS's protections apply however it is turned on)");
            }
            if let Ok(t) = self.schema.resolve_in(&h, Some(&d.config)) {
                if self.schema.nodes.get(&t.node).and_then(|n| n.ty.as_deref()) == Some("package") {
                    for p in t.values.iter().filter(|v| *v != "[" && *v != "]") {
                        self.tree.package(p)?;
                    }
                }
            }
            return self.schema.set(&mut d.config, &h).map_err(|e| as_typed(e, words, &h));
        }
        settings::set(self.tree, &mut d.settings, words).map_err(|e| self.hint(words, e))
    }

    /// `delete <words>`, as on a network device: one value of a list goes alone (`delete system
    /// packages htop`); anything else — a setting, a whole list, a place — takes everything below it,
    /// from every kind of file, and the config files attached there (`delete <place> config [<file>]`:
    /// one, or all of a place's). What is not set is not an error: `Deleted::NotFound`, which the
    /// session says, and a paste goes on.
    pub fn delete(&self, d: &mut Data, words: &[String]) -> Result<Deleted> {
        match self.of(d) {
            Some(schema) => Whole { schema: &schema, tree: self.tree }.delete_in(d, words),
            None => self.delete_in(d, words),
        }
    }

    fn delete_in(&self, d: &mut Data, words: &[String]) -> Result<Deleted> {
        let words: &[String] = &self.spelled(words);
        if software_words(words).is_some() || words == ["system", "software"] || words == ["system", "software", "torq"] {
            bail!("the software's source can be changed (`set system software torq source …`), not deleted: a machine always has one");
        }
        if let Some((place, files)) = self.attachment(words) {
            let key = place.join(" ");
            if files.as_ref().is_some_and(Vec::is_empty) {
                bail!("config [ ]: names no file (`delete {}config` takes them all)", if key.is_empty() { String::new() } else { format!("{key} ") });
            }
            let Some(have) = d.attached.get_mut(&key) else { return Ok(Deleted::NotFound) };
            let Some(files) = files else {
                d.attached.remove(&key);
                return Ok(Deleted::Done);
            };
            // All or nothing, as for a list's values.
            if !files.iter().all(|f| have.contains(f)) {
                return Ok(Deleted::NotFound);
            }
            have.retain(|f| !files.contains(f));
            if have.is_empty() {
                d.attached.remove(&key);
            }
            return Ok(Deleted::Done);
        }
        let hand = self.hand(words, &d.config).ok().flatten();
        // An old name's place (`users dev`): the hand-made schema's container or entry — unless the
        // words are a NixOS place too (`system`, `users`): then they take what `show` shows there.
        let nixos_place = matches!(self.tree.locate(words), Ok((_, None, _)));
        let old_place = self.schema.resolve_in(words, Some(&d.config)).ok().filter(|t| {
            !nixos_place && t.values.is_empty() && !t.node.is_empty() && matches!(self.schema.nodes[&t.node].kind, Kind::Container | Kind::List)
        });
        // Values after the setting's path: those values only.
        let values = match (&hand, &old_place) {
            (Some(h), _) => !self.schema.resolve_in(h, Some(&d.config))?.values.is_empty(),
            (None, Some(_)) => false,
            (None, None) => !self.tree.locate(words).map_err(|e| self.hint(words, e))?.2.is_empty(),
        };
        if values {
            let r = match &hand {
                Some(h) => self.schema.delete(&mut d.config, h).map_err(|e| as_typed(e, words, h)),
                None => settings::delete(self.tree, &mut d.settings, words),
            };
            return match r {
                Ok(()) => Ok(Deleted::Done),
                Err(e) if e.downcast_ref::<crate::tree::NotSet>().is_some() => Ok(Deleted::NotFound),
                Err(e) => Err(e),
            };
        }
        // Everything below: what TorqOS keeps in its own files, by each entry's NixOS path, the rest,
        // and the config files attached there.
        let mut gone = 0;
        let below: Vec<Vec<String>> = self.schema.entries(&d.config).into_iter().map(|(w, _)| w).collect();
        for h in below {
            let nixos = translate(&h, 1, false).unwrap_or_else(|| h.clone());
            // By its NixOS path, by an old place (`users dev`), or by an old name for this one setting
            // (`system packages`: the whole list).
            if nixos.starts_with(words) || old_place.is_some() && h.starts_with(words) || hand.as_ref().is_some_and(|hp| h.starts_with(hp)) {
                self.schema.delete(&mut d.config, &h).map_err(|e| as_typed(e, words, &h))?;
                gone += 1;
            }
        }
        if settings::entries(&d.settings).iter().any(|(w, _)| w.starts_with(words)) {
            // (The whole configuration — `delete` at the top — is every nixos/ file.)
            if words.is_empty() {
                d.settings.clear();
            } else {
                settings::delete(self.tree, &mut d.settings, words)?;
            }
            gone += 1;
        }
        let before = d.attached.len();
        d.attached.retain(|at, _| !at.split(' ').filter(|w| !w.is_empty()).map(String::from).collect::<Vec<_>>().starts_with(words));
        gone += before - d.attached.len();
        Ok(if gone == 0 { Deleted::NotFound } else { Deleted::Done })
    }

    /// Whether `words` name a place (somewhere `edit` can go), not a setting.
    pub fn is_place(&self, words: &[String]) -> bool {
        !words.is_empty() && matches!(self.tree.locate(words), Ok((_, None, _)))
    }

    /// The configuration below `path` in the brace format. An unknown path is an error, not nothing.
    pub fn show(&self, d: &Data, path: &[String]) -> Result<String> {
        let path: &[String] = &self.spelled(path);
        let below = self.below(d, path)?;
        Ok(crate::tree::render_entries(&below))
    }

    /// `| display set` below `path`: each line whole, from the top, so it can be pasted back.
    pub fn display_set(&self, d: &Data, path: &[String]) -> Result<String> {
        let path: &[String] = &self.spelled(path);
        self.below(d, path)?;
        let path = self.shown_path(d, path);
        let all: Vec<(Vec<String>, Val)> = self.entries(d).into_iter().filter(|(w, _)| w.starts_with(&path)).collect();
        Ok(crate::tree::display_set_entries(&all))
    }

    /// The path entries are shown below: an old name's, by its NixOS path.
    fn shown_path(&self, d: &Data, path: &[String]) -> Vec<String> {
        translate(path, 1, true).filter(|_| self.schema.resolve_in(path, Some(&d.config)).is_ok()).unwrap_or_else(|| path.to_vec())
    }

    /// The entries below `path`, the path taken off (the brace format starts where you are).
    fn below(&self, d: &Data, path: &[String]) -> Result<Vec<(Vec<String>, Val)>> {
        let path = self.shown_path(d, path);
        let all = self.entries(d);
        let below: Vec<(Vec<String>, Val)> =
            all.into_iter().filter(|(w, _)| w.starts_with(&path)).map(|(w, v)| (w[path.len()..].to_vec(), v)).collect();
        if below.is_empty() && !path.is_empty() && self.tree.locate(&path).is_err() && translate(&path, 0, false).is_none() {
            bail!("{}: no such setting", path.join(" "));
        }
        // A setting shown by itself is its own statement.
        Ok(below.into_iter().map(|(w, v)| if w.is_empty() { (vec![path.last().cloned().unwrap_or_default()], v) } else { (w, v) }).collect())
    }

    /// What going from `old` to `new` changes, `show | compare` style, by NixOS paths.
    pub fn compare(&self, old: &Data, new: &Data) -> String {
        crate::tree::compare_entries(self.entries(old), self.entries(new))
    }

    /// A NixOS-tree error, with what the old name of the catalog is now.
    fn hint(&self, words: &[String], e: anyhow::Error) -> anyhow::Error {
        let catalog = self.schema.nodes.get("services").and_then(|n| n.values.clone()).unwrap_or_default();
        match words {
            [s, name] if s == "services" && catalog.contains_key(name) => {
                anyhow::anyhow!("{e:#} (TorqOS's catalog: `torq services enabled {name}`)")
            }
            _ => e,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    struct T {
        schema: Schema,
        tree: Tree,
    }
    fn t() -> T {
        T { schema: Schema::parse(include_str!("schema-fixture.json")).unwrap(), tree: Tree::parse(include_str!("nixos-fixture.json")).unwrap() }
    }

    #[test]
    fn one_tree_of_nixos_paths_over_both_kinds_of_files() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in [
            "networking hostName laptop",
            "system time-zone UTC",
            "users users dev packages [ htop ]",
            "system packages vim",
            "virtualisation docker enable true",
            "networking firewall allowedTCPPorts [ 22 ]",
        ] {
            x.set(&mut d, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(d.config.files.len(), 3, "host.json, the system's and the user's packages: {:?}", d.config.files.keys());
        assert_eq!(settings::files(&d.settings).keys().collect::<Vec<_>>(), ["nixos/networking.json", "nixos/virtualisation.json"]);
        assert_eq!(
            x.show(&d, &[]).unwrap(),
            "environment {\n    systemPackages [ vim ];\n}\nnetworking {\n    firewall {\n        allowedTCPPorts [ 22 ];\n    }\n    hostName laptop;\n}\ntime {\n    timeZone UTC;\n}\nusers {\n    users {\n        dev {\n            packages [ htop ];\n        }\n    }\n}\nvirtualisation {\n    docker {\n        enable true;\n    }\n}\n"
        );
        assert_eq!(x.show(&d, &w("networking hostName")).unwrap(), "hostName laptop;\n", "one setting");
        assert_eq!(x.show(&d, &w("system host-name")).unwrap(), "hostName laptop;\n", "by its old name too");
        let files = x.files(&d);
        assert_eq!(files[&w("networking hostName")], "system/host.json");
        assert_eq!(files[&w("networking firewall allowedTCPPorts")], "nixos/networking.json");
        assert_eq!(files[&w("users users dev packages")], "users/dev/packages.json", "a person's own packages: user tier, as before");
        // `| display set` reads back to the same configuration.
        let mut d2 = Data::default();
        for line in x.display_set(&d, &[]).unwrap().lines() {
            x.set(&mut d2, &w(line.strip_prefix("set ").unwrap())).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(d2, d);
    }

    /// What is not set is `NotFound` (nothing changed, no error), wherever it would be; an unknown
    /// path is still an error.
    #[test]
    fn deletes_are_exact() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in ["virtualisation docker enable false", "networking firewall allowedTCPPorts [ 22 80 ]", "config hardware-configuration.nix"] {
            x.set(&mut d, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        let before = d.clone();
        // A value named must be the value set; every value of a list named must be there.
        assert_eq!(x.delete(&mut d, &w("virtualisation docker enable true")).unwrap(), Deleted::NotFound);
        assert_eq!(x.delete(&mut d, &w("networking firewall allowedTCPPorts [ 22 9999 ]")).unwrap(), Deleted::NotFound);
        assert_eq!(d, before, "nothing deleted");
        assert_eq!(x.delete(&mut d, &w("virtualisation docker enable false")).unwrap(), Deleted::Done);
        // `config [ ]` names no file: an error, never "all of them".
        assert!(format!("{:#}", x.delete(&mut d, &w("config [ ]")).unwrap_err()).contains("names no file"));
        assert!(x.set(&mut d, &w("config [ ]")).is_err());
        assert_eq!(attach::files(&d.attached).count(), 1);
        // A word that is a NixOS place takes what `show` shows there, not old names under it.
        x.set(&mut d, &w("system time-zone UTC")).unwrap();
        assert_eq!(x.delete(&mut d, &w("system")).unwrap(), Deleted::NotFound);
        assert!(x.show(&d, &w("time")).unwrap().contains("UTC"));
    }

    #[test]
    fn deleting_what_is_not_set_finds_nothing() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in ["networking firewall allowedTCPPorts [ 22 ]", "system packages [ htop ]", "config hardware-configuration.nix"] {
            x.set(&mut d, &w(line)).unwrap();
        }
        let before = d.clone();
        for line in [
            "networking hostName",
            "system host-name",
            "virtualisation docker",
            "virtualisation",
            "networking firewall allowedTCPPorts 80",
            "system packages vim",
            "virtualisation config",
            "config other.nix",
        ] {
            assert_eq!(x.delete(&mut d, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}")), Deleted::NotFound, "{line}");
            assert_eq!(d, before, "{line}: nothing changed");
        }
        assert!(format!("{:#}", x.delete(&mut d, &w("nonsense")).unwrap_err()).contains("no such setting"));
    }

    /// A place holds several config files: `set` adds (never replaces), `[ a b ]` adds several,
    /// `delete <place> config <file>` takes one, `delete <place> config` all of them.
    #[test]
    fn config_files_add_and_go_one_or_all() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        x.set(&mut d, &w("config hardware-configuration.nix")).unwrap();
        x.set(&mut d, &w("config custom/motd.nix")).unwrap();
        assert_eq!(attach::files(&d.attached).collect::<Vec<_>>(), ["custom/motd.nix", "hardware-configuration.nix"], "added, not replaced");
        assert_eq!(x.show(&d, &[]).unwrap(), "config [ custom/motd.nix hardware-configuration.nix ];\n");
        x.set(&mut d, &w("services httpd config [ a.nix b.nix ]")).unwrap();
        assert_eq!(d.attached["services httpd"].len(), 2);
        x.set(&mut d, &w("services httpd config a.nix")).unwrap();
        assert_eq!(d.attached["services httpd"].len(), 2, "once each");
        assert_eq!(x.delete(&mut d, &w("config custom/motd.nix")).unwrap(), Deleted::Done);
        assert_eq!(x.show(&d, &[]).unwrap().lines().next(), Some("config hardware-configuration.nix;"), "one shows as one");
        assert_eq!(x.delete(&mut d, &w("services httpd config [ a.nix nope.nix ]")).unwrap(), Deleted::NotFound, "all or nothing");
        assert_eq!(d.attached["services httpd"].len(), 2);
        assert_eq!(x.delete(&mut d, &w("services httpd config")).unwrap(), Deleted::Done);
        assert!(!d.attached.contains_key("services httpd"));
        assert!(x.set(&mut d, &w("config")).is_err(), "which file");
        assert!(x.set(&mut d, &w("config ../x.nix")).is_err());
    }

    #[test]
    fn deleting_a_whole_place_and_the_catalog_says_its_new_name() {
        let t = t();
        let x = Whole { schema: &t.schema, tree: &t.tree };
        let mut d = Data::default();
        for line in ["virtualisation docker enable true", "virtualisation docker rootless enable true", "networking firewall allowedTCPPorts [ 22 80 ]", "users users dev packages [ htop jq ]"] {
            x.set(&mut d, &w(line)).unwrap();
        }
        // A place, or a whole list, goes whole (as on a network device); one value alone.
        let mut whole_place = d.clone();
        assert_eq!(x.delete(&mut whole_place, &w("virtualisation docker")).unwrap(), Deleted::Done);
        assert!(!x.show(&whole_place, &[]).unwrap().contains("docker"));
        let mut whole_list = d.clone();
        assert_eq!(x.delete(&mut whole_list, &w("networking firewall allowedTCPPorts")).unwrap(), Deleted::Done);
        assert!(!x.show(&whole_list, &[]).unwrap().contains("allowedTCPPorts"));
        x.delete(&mut d, &w("networking firewall allowedTCPPorts 80")).unwrap();
        x.delete(&mut d, &w("virtualisation docker rootless")).unwrap();
        x.delete(&mut d, &w("users users dev packages jq")).unwrap();
        assert_eq!(x.display_set(&d, &[]).unwrap(), "set networking firewall allowedTCPPorts 22\nset users users dev packages htop\nset virtualisation docker enable true\n");
        let e = format!("{:#}", x.set(&mut d, &w("services ssh")).unwrap_err());
        assert!(e.contains("services sshd: not a setting by itself: `set services sshd enable true` turns it on"), "{e}");
        assert!(x.set(&mut d, &w("services printing enable true")).is_ok(), "NixOS's own services are NixOS's");
        assert!(x.is_place(&w("virtualisation docker")) && x.is_place(&w("services httpd virtualHosts example.com")));
        assert!(!x.is_place(&w("virtualisation docker enable")));
    }

    #[test]
    fn the_map_is_the_hand_made_schemas() {
        // Every file-backed setting of the hand-made schema has its NixOS path, and that path is a
        // setting of the NixOS tree: nothing TorqOS keeps in its own files is missing from the tree.
        let t = t();
        for (key, node) in &t.schema.nodes {
            if node.file.is_none() {
                continue;
            }
            let hand: Vec<String> = key.split(' ').map(|p| if p == "<name>" { "dev".to_string() } else { p.to_string() }).collect();
            let nixos = translate(&hand, 1, false).unwrap_or_else(|| panic!("{key}: no NixOS path in MAP"));
            assert!(t.tree.locate(&nixos).is_ok(), "{key} → {}: not a NixOS setting", nixos.join(" "));
        }
    }
}

#[cfg(test)]
#[test]
fn a_person_added_in_the_candidate_can_have_software_in_it() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let mut d = Data::default();
    assert!(format!("{:#}", x.set(&mut d, &w("users users alice packages btop")).unwrap_err()).contains("'alice' is not one of the users on this machine (dev); a new person first: `set users users alice isNormalUser true`"));
    x.set(&mut d, &w("users users alice isNormalUser true")).unwrap();
    x.set(&mut d, &w("users users alice packages btop")).unwrap();
    assert!(x.show(&d, &w("users users alice")).unwrap().contains("packages [ btop ];"));
    assert_eq!(x.delete(&mut d, &w("users users alice packages btop")).unwrap(), Deleted::Done);
}

#[cfg(test)]
#[test]
fn a_password_is_never_in_the_configuration() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let e = format!("{:#}", x.set(&mut Data::default(), &w("users users dev hashedPassword $6$x$y")).unwrap_err());
    assert_eq!(e, "users users dev hashedPassword: a password is never in the configuration (everyone can read it, and its history keeps it): `request system password dev` sets one");
    assert!(!tree.next(&w("users users dev")).iter().any(|(x, _)| x == "hashedPassword") || tree.password(&w("users users dev hashedPassword")), "known as one");
    // The same list as the evaluation's (settings-fence.nix).
    let Ok(fence) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../modules/settings-fence.nix")) else { return };
    for p in ["password", "hashedPassword", "initialPassword", "initialHashedPassword", "hashedPasswordFile", "passwordFile"] {
        assert!(fence.contains(&format!("\"{p}\"")) && tree.password(&w(&format!("users users anyone {p}"))), "{p}");
    }
}

#[cfg(test)]
#[test]
fn an_abbreviated_setting_is_kept_where_the_whole_name_is() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let mut d = Data::default();
    x.set(&mut d, &w("networking hostName baz")).unwrap();
    x.set(&mut d, &w("networkin hostNa qux")).unwrap();
    assert_eq!(x.show(&d, &w("networking")).unwrap(), "hostName qux;\n", "one setting, one place, however it was typed");
    assert!(d.settings.is_empty(), "TorqOS's own file, both times: {:?}", d.settings);
    assert_eq!(x.show(&d, &w("netw")).unwrap(), x.show(&d, &w("networking")).unwrap());
    assert_eq!(x.delete(&mut d, &w("netw hostN")).unwrap(), Deleted::Done);
}

#[cfg(test)]
#[test]
fn an_error_names_the_setting_as_it_was_typed() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    let e = |line: &str| format!("{:#}", x.set(&mut Data::default(), &w(line)).unwrap_err());
    assert_eq!(e("networking hostName my box"), "networking hostName: takes exactly one value");
    assert_eq!(e("system host-name my box"), "system host-name: takes exactly one value", "the old name, as typed");
    assert!(e("networking hostName my_box!").starts_with("'my_box!' is not a valid networking hostName: letters, digits and -"), "{}", e("networking hostName my_box!"));
}

#[cfg(test)]
#[test]
fn the_module_keeps_them_out_of_nixos_too() {
    // modules/settings-fence.nix's `elsewhere` must name every setting MAP keeps in TorqOS's own files
    // (`*` where an entry is named): the evaluation refuses them in nixos/, whoever wrote it.
    // Read where the repo is (the dev shell); the package's own build has only cli/, and skips it.
    let Ok(fence) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../modules/settings-fence.nix")) else { return };
    for (nixos, _, _) in MAP {
        let words: Vec<&str> = nixos.split(' ').map(|w| if w == "<name>" { "*" } else { w }).collect();
        let pat = format!("[ {} ]", words.iter().map(|w| format!("\"{w}\"")).collect::<Vec<_>>().join(" "));
        assert!(fence.contains(&pat), "settings-fence.nix's elsewhere has no {pat}");
    }
}

#[cfg(test)]
#[test]
fn review_of_s3c_one_place_one_value_and_all_means_all() {
    let (schema, tree) = (Schema::parse(include_str!("schema-fixture.json")).unwrap(), Tree::parse(include_str!("nixos-fixture.json")).unwrap());
    let x = Whole { schema: &schema, tree: &tree };
    let w = |s: &str| crate::session::words(s).unwrap();
    let mut d = Data::default();
    for line in ["networking hostName laptop", "networking firewall allowedTCPPorts [ 22 ]", "users users dev packages [ htop ]", "system packages vim", "system time-zone UTC"] {
        x.set(&mut d, &w(line)).unwrap();
    }
    // A whole list by its old name, as by its NixOS one (`delete system packages`).
    for name in ["system packages", "environment systemPackages"] {
        let mut e = d.clone();
        x.set(&mut e, &w("system packages [ htop vim ]")).unwrap();
        assert_eq!(x.delete(&mut e, &w(name)).unwrap_or_else(|err| panic!("{name}: {err:#}")), Deleted::Done);
        assert!(!x.show(&e, &[]).unwrap().contains("vim"), "{name}");
    }
    // `delete networking`: everything shown below it, from both kinds of files.
    x.delete(&mut d, &w("networking")).unwrap();
    assert!(!x.show(&d, &[]).unwrap().contains("networking"), "{}", x.show(&d, &[]).unwrap());
    // An old name's place deletes what it always did.
    x.delete(&mut d, &w("users dev")).unwrap();
    assert!(!x.show(&d, &[]).unwrap().contains("dev"));
    // A word that is a NixOS place takes what `show` shows there — not the old names that began with
    // it (`system host-name` is `networking hostName` now; `system time-zone` is `time timeZone`).
    let shown = x.show(&d, &w("system")).unwrap_or_default();
    assert_eq!(x.delete(&mut d, &w("system")).unwrap(), if shown.is_empty() { Deleted::NotFound } else { Deleted::Done });
    assert!(x.show(&d, &[]).unwrap().contains("timeZone UTC") && x.show(&d, &[]).unwrap().contains("systemPackages [ vim ]"), "{}", x.show(&d, &[]).unwrap());
    x.delete(&mut d, &w("time")).unwrap();
    x.delete(&mut d, &w("environment")).unwrap();
    x.delete(&mut d, &w("torq")).unwrap();
    assert_eq!(x.show(&d, &[]).unwrap(), "");
    assert_eq!(x.delete(&mut d, &w("virtualisation")).unwrap(), Deleted::NotFound, "nothing set below it: nothing deleted, no error");
    // Text that would draw lines of its own: refused when set, and when read.
    let e = format!("{:#}", x.set(&mut d, &[w("services httpd virtualHosts a documentRoot"), vec!["/x\nKeys that will".into()]].concat()).unwrap_err());
    assert!(e.contains("multi-line text goes in a config file"), "{e}");
    let e = format!("{:#}", settings::load([("nixos/environment.json", br#"{"variables":{"X":"a\nb"}}"# as &[u8])]).unwrap_err());
    assert!(e.contains("line break or another control character"), "{e}");
    // One setting, one place: a kept one in a nixos/ file is refused, naming where it lives.
    let e = format!("{:#}", settings::load([("nixos/networking.json", br#"{"hostName":"x"}"# as &[u8])]).unwrap_err());
    assert!(e.contains("networking hostName is kept in its own file (`system host-name`)"), "{e}");
    let e = format!("{:#}", settings::load([("nixos/environment.json", br#"{"systemPackages":["vim"]}"# as &[u8])]).unwrap_err());
    assert!(e.contains("environment systemPackages is kept in its own file (`system packages`)"), "{e}");
    let e = format!("{:#}", settings::load([("nixos/users.json", br#"{"users":{"dev":{"packages":["jq"]}}}"# as &[u8])]).unwrap_err());
    assert!(e.contains("users users dev packages is kept in its own file (`users <name> packages`)"), "a person's, named: {e}");
}
