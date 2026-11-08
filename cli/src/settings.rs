//! The configuration tree's NixOS settings (docs/CONFIG-IS-THE-DEVICE.md): the config repo's
//! `nixos/<top-level>.json` files (applied by modules/nixos-settings.nix), as one value per NixOS
//! top-level name, shaped like the options. `set` and `delete` act on them by NixOS paths, checked
//! against the system's own settings (nixos.rs) and its fence; the brace format, `| display set` and
//! `show | compare` see them as entries, like the rest of the tree (tree.rs).
//!
//! Pure: it reads and writes file contents it is given. Every file here is system tier.

use crate::nixos::Tree;
use crate::tree::Val;
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The folder of the tree's NixOS settings in the config repo.
pub const DIR: &str = "nixos/";

/// The values, by NixOS top-level name (`virtualisation` ← nixos/virtualisation.json).
pub type Settings = BTreeMap<String, Value>;

/// The top-level name a config repo path holds, if it is one of ours.
pub fn top_of(path: &str) -> Option<&str> {
    let name = path.strip_prefix(DIR)?.strip_suffix(".json")?;
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(name)
}

/// The first text with a control character (a line break, an escape) in a value, by path: such text
/// would draw lines of its own in `show`, `show | compare` and an approval question, and does not
/// read back from `| display set`. Multi-line text belongs in a config file.
fn control_text(path: &mut Vec<String>, v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.chars().any(char::is_control) => Some(path.join(" ")),
        Value::Array(a) => a.iter().find_map(|x| control_text(path, x)),
        Value::Object(o) => o.iter().find_map(|(k, x)| {
            if k.chars().any(char::is_control) {
                return Some(path.join(" "));
            }
            path.push(k.clone());
            let r = control_text(path, x);
            path.pop();
            r
        }),
        _ => None,
    }
}

/// The first of the settings TorqOS keeps in its own files (whole.rs MAP) set in a nixos/ file too:
/// one setting, one place — two would show one value and build another.
fn kept_elsewhere(top: &str, v: &Value) -> Option<(String, String)> {
    for (nixos, hand, _) in crate::whole::MAP {
        let words: Vec<&str> = nixos.split(' ').collect();
        if words[0] != top {
            continue;
        }
        // Each place the words reach, with its path (an entry's own name where `<name>` stands).
        let mut at: Vec<(Vec<String>, &Value)> = vec![(vec![top.to_string()], v)];
        for w in &words[1..] {
            at = at
                .into_iter()
                .flat_map(|(path, x)| match (x, *w) {
                    (Value::Object(o), "<name>") => o.iter().map(|(k, y)| ([path.clone(), vec![k.clone()]].concat(), y)).collect::<Vec<_>>(),
                    (Value::Object(o), k) => o.get(k).map(|y| ([path.clone(), vec![k.to_string()]].concat(), y)).into_iter().collect(),
                    _ => vec![],
                })
                .collect();
        }
        if let Some((path, _)) = at.first() {
            return Some((path.join(" "), hand.to_string()));
        }
    }
    None
}

/// Read the files (path, content) that are ours. Each must be a JSON object: anything else is
/// refused, naming the file — the shell must not hide, or rewrite, what it cannot show.
pub fn load<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Settings> {
    let mut s = Settings::new();
    for (path, bytes) in files {
        let Some(top) = top_of(path) else { continue };
        let v: Value = serde_json::from_slice(bytes).with_context(|| format!("{path}: not JSON; fix the file (as raw JSON) first"))?;
        if !v.is_object() {
            bail!("{path}: not a JSON object; fix the file (as raw JSON) first");
        }
        if let Some(at) = control_text(&mut vec![top.to_string()], &v) {
            bail!("{path}: {at} holds a line break or another control character; torqcli cannot show it faithfully — fix the file (multi-line text goes in a config file)");
        }
        if let Some((nixos, hand)) = kept_elsewhere(top, &v) {
            bail!("{path}: {nixos} is kept in its own file (`{hand}`), not here; remove it from {path}");
        }
        s.insert(top.to_string(), v);
    }
    Ok(s)
}

/// The files as TorqOS writes them (path → content): keys sorted, two-space indent, a final newline.
/// An empty top-level has no file.
pub fn files(s: &Settings) -> BTreeMap<String, String> {
    s.iter()
        .filter(|(_, v)| v.as_object().is_some_and(|o| !o.is_empty()))
        .map(|(top, v)| (format!("{DIR}{top}.json"), format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default())))
        .collect()
}

/// A scalar as the tree shows it.
fn word(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Every value as an entry (path, value), for the brace format, `| display set` and compare.
pub fn entries(s: &Settings) -> Vec<(Vec<String>, Val)> {
    fn walk(path: &mut Vec<String>, v: &Value, out: &mut Vec<(Vec<String>, Val)>) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    path.push(k.clone());
                    walk(path, x, out);
                    path.pop();
                }
            }
            // A list of entries (each with settings of its own): each by its number, from 1.
            Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object) => {
                for (i, x) in a.iter().enumerate() {
                    path.push((i + 1).to_string());
                    walk(path, x, out);
                    path.pop();
                }
            }
            Value::Array(a) => out.push((path.clone(), Val::Many(a.iter().map(word).collect()))),
            other => out.push((path.clone(), Val::One(word(other)))),
        }
    }
    let mut out = Vec::new();
    for (top, v) in s {
        walk(&mut vec![top.clone()], v, &mut out);
    }
    out
}

/// `set <words>`: the setting the words name, given the value after them (a list gains its
/// elements; a set of plain values gains its entry; anything else is replaced).
pub fn set(tree: &Tree, s: &mut Settings, words: &[String]) -> Result<()> {
    let r = tree.resolve(words)?;
    // `all` as an entry's name would be ambiguous (`delete … virtualHosts all`).
    if let Some(i) = tree.entry_positions(&r.loc).into_iter().find(|i| crate::syntax::RESERVED.contains(&r.loc[*i].as_str())) {
        bail!("'{}' cannot be named in {}: torqcli keeps the word for a command", r.loc[i], r.loc[..i].join(" "));
    }
    if let Some(k) = r.loc.iter().find(|k| k.starts_with('_')) {
        bail!("{}: a name starting with _ is a NixOS module directive, not a setting: the tree holds data only", k);
    }
    // A backup's key or a repository's credentials (settings-fence.nix, rule 3), said as such.
    if tree.password(&r.loc) && r.loc.first().is_some_and(|w| w != "users") {
        if r.loc.iter().any(|w| w == "rcloneConfig") {
            bail!("{}: a repository's credentials are never in the configuration (everyone can read it): put them in a file of root's and point rcloneConfigFile or environmentFile at it", r.loc.join(" "));
        }
        bail!("{}: a backup's key is never in the configuration, nor pointed at from it: it is kept root's alone, outside it (`request system backup key <name>` gives it)", r.loc.join(" "));
    }
    if tree.password(&r.loc) {
        bail!("{}: a password is never in the configuration (everyone can read it, and its history keeps it): `request system password {}` sets one", r.loc.join(" "), r.loc.get(2).map_or("<user>", String::as_str));
    }
    if tree.fenced(&r.loc) {
        bail!("{}: not settable in the configuration tree (it reaches the broker or TorqOS itself)", r.loc.join(" "));
    }
    // A value that is not one: said with what the setting is (its help often says the form).
    let new = crate::nixos::value(r.ty, &r.values).map_err(|e| anyhow::anyhow!("{}: {e:#}{}", r.loc.join(" "), crate::complete::about(r.help)))?;
    for p in crate::nixos::packages_in(r.ty, &new) {
        tree.package(p).with_context(|| r.loc.join(" "))?;
    }
    if control_text(&mut r.loc.clone(), &new).is_some() {
        bail!("{}: a line break or another control character in a value: multi-line text goes in a config file", r.loc.join(" "));
    }
    let items = tree.item_positions(&r.loc);
    let (top, rest) = r.loc.split_first().context("a setting has a path")?;
    let mut at = s.entry(top.clone()).or_insert_with(|| Value::Object(Map::new()));
    for (j, k) in rest.iter().enumerate() {
        if items.contains(&(j + 1)) {
            // A list of entries: by number; the next one adds an entry.
            if at.as_object().is_some_and(Map::is_empty) {
                *at = Value::Array(Vec::new());
            }
            let list = at.as_array_mut().with_context(|| format!("{}: holds a value, not a list of entries", r.loc[..=j].join(" ")))?;
            let (i, len): (usize, usize) = (k.parse().context("an entry's number")?, list.len());
            if i == len + 1 {
                list.push(Value::Object(Map::new()));
            } else if i > len + 1 {
                bail!("{}: {} — {} adds one", r.loc[..=j].join(" "), match len { 0 => "it has no entries".to_string(), 1 => "it has 1 entry".to_string(), n => format!("it has {n} entries") }, len + 1);
            }
            at = &mut list[i - 1];
        } else {
            let o = at.as_object_mut().with_context(|| format!("{}: holds a value, not settings", r.loc.join(" ")))?;
            at = o.entry(k.clone()).or_insert_with(|| Value::Object(Map::new()));
        }
    }
    match (at, new) {
        (Value::Array(have), Value::Array(add)) => {
            for x in add {
                if !have.contains(&x) {
                    have.push(x);
                }
            }
        }
        (Value::Object(have), Value::Object(add)) if !have.is_empty() => have.extend(add),
        (slot, v) => *slot = v,
    }
    Ok(())
}

/// `delete <words>`: a setting, a value of a list, or a whole place (`all`, checked by the caller).
/// What is left empty above it goes too.
pub fn delete(tree: &Tree, s: &mut Settings, words: &[String]) -> Result<()> {
    let (loc, ty, values) = tree.locate(words)?;
    if tree.fenced(&loc) {
        bail!("{}: not changeable in the configuration tree", loc.join(" "));
    }
    let remove = match (ty, values.is_empty()) {
        (Some(t), false) => Some(crate::nixos::value(t, &values).with_context(|| loc.join(" "))?),
        _ => None,
    };
    let (top, rest) = loc.split_first().context("a path")?;
    // A whole top-level place: its file.
    if rest.is_empty() {
        return match s.remove(top) {
            Some(_) => Ok(()),
            None => Err(crate::tree::NotSet(loc.join(" ")).into()),
        };
    }
    let Some(root) = s.get_mut(top) else { return Err(crate::tree::NotSet(loc.join(" ")).into()) };
    if !remove_at(root, rest, remove.as_ref())? {
        return Err(crate::tree::NotSet(loc.join(" ")).into());
    }
    if root.as_object().is_some_and(|o| o.is_empty()) {
        s.remove(top);
    }
    Ok(())
}

/// Remove `path` below `v` (with `what`: only those list elements, or that entry of a set). Whether
/// anything was there; objects left empty are removed on the way back.
fn remove_at(v: &mut Value, path: &[String], what: Option<&Value>) -> Result<bool> {
    let Some((k, rest)) = path.split_first() else { return Ok(false) };
    // A list of entries, by number: the entry (those after it renumber), or what is inside it.
    if let Value::Array(list) = v {
        let Some(i) = k.parse::<usize>().ok().filter(|i| (1..=list.len()).contains(i)) else { return Ok(false) };
        if rest.is_empty() {
            list.remove(i - 1);
            return Ok(true);
        }
        let found = remove_at(&mut list[i - 1], rest, what)?;
        if list[i - 1].as_object().is_some_and(Map::is_empty) {
            list.remove(i - 1);
        }
        return Ok(found);
    }
    let Some(o) = v.as_object_mut() else { return Ok(false) };
    let found = if rest.is_empty() {
        match (o.get_mut(k), what) {
            (None, _) => false,
            // Values named: all of them must be there (all or nothing, as for the files TorqOS keeps).
            (Some(Value::Array(have)), Some(Value::Array(gone))) => {
                if !gone.iter().all(|g| have.contains(g)) {
                    return Ok(false);
                }
                have.retain(|x| !gone.contains(x));
                if have.is_empty() {
                    o.remove(k);
                }
                true
            }
            (Some(Value::Object(have)), Some(Value::Object(gone))) => {
                if !gone.keys().all(|k| have.contains_key(k)) {
                    return Ok(false);
                }
                have.retain(|key, _| !gone.contains_key(key));
                if have.is_empty() {
                    o.remove(k);
                }
                true
            }
            // One value named: only that value goes (`delete … enable true` when it is false: nothing).
            (Some(have), Some(want)) if have != want => false,
            (Some(_), _) => o.remove(k).is_some(),
        }
    } else {
        match o.get_mut(k) {
            Some(child) => {
                let found = remove_at(child, rest, what)?;
                if child.as_object().is_some_and(|c| c.is_empty()) || child.as_array().is_some_and(|c| c.is_empty()) {
                    o.remove(k);
                }
                found
            }
            None => false,
        }
    };
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Tree {
        Tree::parse(include_str!("nixos-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }

    /// A list of entries with no names (an interface's addresses): each by its number; the next one
    /// adds an entry, one past it is refused; deleting one renumbers those after it; in the files, a
    /// JSON list, as NixOS reads it.
    #[test]
    fn a_list_of_entries_by_number() {
        let t = tree();
        let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let mut s = Settings::new();
        for line in ["networking interfaces eth0 ipv4 addresses 1 address 10.0.0.1", "networking interfaces eth0 ipv4 addresses 1 prefixLength 24", "networking interfaces eth0 ipv4 addresses 2 address 10.0.0.2"] {
            set(&t, &mut s, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(s["networking"]["interfaces"]["eth0"]["ipv4"]["addresses"], serde_json::json!([{ "address": "10.0.0.1", "prefixLength": 24 }, { "address": "10.0.0.2" }]));
        let e = format!("{:#}", set(&t, &mut s, &w("networking interfaces eth0 ipv4 addresses 9 address 10.0.0.9")).unwrap_err());
        assert_eq!(e, "networking interfaces eth0 ipv4 addresses: it has 2 entries — 3 adds one");
        assert!(format!("{:#}", set(&t, &mut s, &w("networking interfaces eth0 ipv4 addresses x address 10.0.0.9")).unwrap_err()).contains("'x' is not an entry's number"));
        let shown: Vec<String> = entries(&s).into_iter().map(|(p, _)| p.join(" ")).collect();
        assert_eq!(shown, ["networking interfaces eth0 ipv4 addresses 1 address", "networking interfaces eth0 ipv4 addresses 1 prefixLength", "networking interfaces eth0 ipv4 addresses 2 address"]);
        delete(&t, &mut s, &w("networking interfaces eth0 ipv4 addresses 1")).unwrap();
        assert_eq!(s["networking"]["interfaces"]["eth0"]["ipv4"]["addresses"], serde_json::json!([{ "address": "10.0.0.2" }]), "the one after it is 1 now");
        delete(&t, &mut s, &w("networking interfaces eth0 ipv4 addresses 1 address")).unwrap();
        assert!(!s.contains_key("networking"), "what is left empty goes too");
    }

    #[test]
    fn set_writes_files_shaped_like_the_options() {
        let t = tree();
        let mut s = Settings::new();
        for line in [
            "virtualisation docker enable true",
            "services httpd virtualHosts example.com forceSSL true",
            "services httpd virtualHosts example.com serverAliases [ www.example.com ]",
            "networking firewall allowedTCPPorts [ 80 443 ]",
            "networking firewall allowedTCPPorts 8080",
            "networking firewall allowedTCPPorts 80",
            "environment variables EDITOR vim",
            "environment variables PAGER less",
        ] {
            set(&t, &mut s, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        let f = files(&s);
        assert_eq!(f["nixos/virtualisation.json"], "{\n  \"docker\": {\n    \"enable\": true\n  }\n}\n");
        let net: Value = serde_json::from_str(&f["nixos/networking.json"]).unwrap();
        assert_eq!(net, serde_json::json!({"firewall": {"allowedTCPPorts": [80, 443, 8080]}}), "a list gains, without repeats");
        let env: Value = serde_json::from_str(&f["nixos/environment.json"]).unwrap();
        assert_eq!(env, serde_json::json!({"variables": {"EDITOR": "vim", "PAGER": "less"}}));
        let back = load(f.iter().map(|(p, c)| (p.as_str(), c.as_bytes()))).unwrap();
        assert_eq!(back, s, "what is written reads back");
    }

    #[test]
    fn entries_render_and_round_trip_through_display_set() {
        let t = tree();
        let mut s = Settings::new();
        for line in ["virtualisation docker enable true", "networking firewall allowedTCPPorts [ 80 443 ]", "services httpd virtualHosts example.com forceSSL true"] {
            set(&t, &mut s, &w(line)).unwrap();
        }
        let e = entries(&s);
        assert_eq!(
            crate::tree::render_entries(&e),
            "networking {\n    firewall {\n        allowedTCPPorts [ 80 443 ];\n    }\n}\nservices {\n    httpd {\n        virtualHosts {\n            example.com {\n                forceSSL true;\n            }\n        }\n    }\n}\nvirtualisation {\n    docker {\n        enable true;\n    }\n}\n"
        );
        // `| display set`, run again, makes the same settings.
        let mut again = Settings::new();
        for line in crate::tree::display_set_entries(&e).lines() {
            set(&t, &mut again, &w(line.strip_prefix("set ").unwrap())).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
        assert_eq!(again, s);
    }

    #[test]
    fn delete_takes_exactly_what_it_names() {
        let t = tree();
        let mut s = Settings::new();
        for line in ["networking firewall allowedTCPPorts [ 80 443 ]", "virtualisation docker enable true", "virtualisation docker rootless enable true", "environment variables EDITOR vim"] {
            set(&t, &mut s, &w(line)).unwrap();
        }
        delete(&t, &mut s, &w("networking firewall allowedTCPPorts 80")).unwrap();
        assert_eq!(s["networking"], serde_json::json!({"firewall": {"allowedTCPPorts": [443]}}));
        delete(&t, &mut s, &w("networking firewall allowedTCPPorts 443")).unwrap();
        assert!(!s.contains_key("networking"), "nothing left: no file");
        delete(&t, &mut s, &w("virtualisation docker rootless")).unwrap();
        assert_eq!(s["virtualisation"], serde_json::json!({"docker": {"enable": true}}), "a place, and what is below it");
        delete(&t, &mut s, &w("environment variables EDITOR vim")).unwrap();
        assert!(!s.contains_key("environment"));
        let e = format!("{:#}", delete(&t, &mut s, &w("virtualisation docker rootless enable")).unwrap_err());
        assert!(e.contains("not set"), "{e}");
    }

    #[test]
    fn the_fence_and_bad_files() {
        let t = tree();
        let mut s = Settings::new();
        for line in ["torq broker rebuildCommand x", "environment etc torq/broker.json text x", "systemd services torq-broker-init enable false"] {
            let e = set(&t, &mut s, &w(line)).map(|_| ()).unwrap_err();
            assert!(format!("{e:#}").contains("not settable in the configuration tree"), "{line}: {e:#}");
        }
        assert!(s.is_empty());
        assert!(format!("{:#}", set(&t, &mut s, &w("services httpd virtualHosts _x forceSSL true")).unwrap_err()).contains("the tree holds data only"));
        set(&t, &mut s, &w("environment etc motd-torq/x text hi")).unwrap();
        set(&t, &mut s, &w("systemd services myapp enable true")).unwrap();
        s.clear();
        assert!(format!("{:#}", load([("nixos/x.json", b"[1]" as &[u8])]).unwrap_err()).contains("not a JSON object"));
        assert!(load([("nixos/x.json", b"{" as &[u8])]).is_err());
        assert_eq!(load([("nixos/../x.json", b"{}" as &[u8]), ("other.json", b"{}")]).unwrap(), Settings::new(), "not ours");
    }
}
