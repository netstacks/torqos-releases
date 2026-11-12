//! The configuration tree (ADR-016, docs/CLI-DESIGN.md): the schema TorqOS installs as
//! /etc/torq/schema.json (modules/schema.nix), the tree's data as the config repo files it lives in,
//! `set` / `delete` in J syntax, and the views of it: `show configuration`, `| display set`,
//! `| compare`.
//!
//! Pure: no I/O but parsing what it is given. The modules' checks stay the authority on what is
//! valid; the checks here are for early, clear errors.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// What `delete` names is not set: nothing to delete. Its own error type, so the caller tells it
/// from a real mistake (a delete of what is not there warns and goes on, as on a network device).
#[derive(Debug)]
pub struct NotSet(pub String);

impl std::fmt::Display for NotSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not set", self.0)
    }
}

impl std::error::Error for NotSet {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Container,
    List,
    Leaf,
    LeafList,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    pub kind: Kind,
    pub help: String,
    pub file: Option<String>,
    pub key: Option<String>,
    #[serde(default = "system")]
    pub tier: String,
    pub values: Option<BTreeMap<String, String>>,
    #[serde(rename = "type")]
    pub ty: Option<String>,
}

fn system() -> String {
    "system".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct Schema {
    pub version: u32,
    pub nodes: BTreeMap<String, Node>,
}

/// The tree's data: the config repo files it lives in, by path. A leaf's file is a JSON object of
/// strings, a leaf-list's a JSON list of strings. An absent file is an empty one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub files: BTreeMap<String, Data>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Data {
    Object(BTreeMap<String, String>),
    List(Vec<String>),
}

impl Data {
    /// The file's content as TorqOS writes it: keys sorted, lists sorted and de-duplicated, one item
    /// per line (so diffs stay small), a final newline.
    pub fn render(&self) -> String {
        let v = match self {
            Data::Object(o) => serde_json::to_value(o),
            Data::List(l) => serde_json::to_value(l.iter().collect::<BTreeSet<_>>()),
        }
        .expect("strings serialise");
        match self {
            Data::List(l) if l.is_empty() => "[]\n".into(),
            _ => format!("{}\n", serde_json::to_string_pretty(&v).expect("strings serialise")),
        }
    }
}

/// A command's words resolved against the schema.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// The schema node: its key, e.g. "users <name> packages".
    pub node: String,
    /// The words naming it, e.g. ["users", "alice", "packages"].
    pub words: Vec<String>,
    /// The list entry name, if the path passes through a list (`users alice …`).
    pub name: Option<String>,
    /// The words after the node: a leaf's value, a leaf-list's elements.
    pub values: Vec<String>,
}

/// What one entry of the tree holds: a leaf's value, or a leaf-list's elements.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    One(String),
    Many(Vec<String>),
}

impl Schema {
    pub fn parse(json: &str) -> Result<Schema> {
        let s: Schema = serde_json::from_str(json).context("reading the TorqOS schema")?;
        if s.version != 1 {
            bail!("this torqcli reads schema version 1, not {}", s.version);
        }
        // No setting may be named `all`: `delete <path> all` would mean two things.
        if let Some(key) = s.nodes.keys().find(|k| k.split(' ').any(|w| crate::syntax::RESERVED.contains(&w))) {
            bail!("schema: '{key}' uses a word torqcli keeps for commands");
        }
        // Each file is one kind of thing: a leaf-list's alone, or leaves' (an object). No two file
        // patterns may match the same file unless they are the same pattern for leaves.
        let files: Vec<(&String, &Node)> = s.nodes.iter().filter(|(_, n)| n.file.is_some()).collect();
        for (i, (ka, a)) in files.iter().enumerate() {
            for (kb, b) in &files[i + 1..] {
                let (pa, pb) = (a.file.as_deref().unwrap(), b.file.as_deref().unwrap());
                let overlap = matches_file(pa, &pb.replace("<name>", "x")).is_some() || matches_file(pb, &pa.replace("<name>", "x")).is_some();
                let both_leaves = a.kind == Kind::Leaf && b.kind == Kind::Leaf && pa == pb;
                if overlap && !both_leaves {
                    bail!("schema: {ka} and {kb} would share a file ({pa}, {pb})");
                }
            }
        }
        Ok(s)
    }

    /// Whether `path` is one of the tree's files.
    pub fn describes(&self, path: &str) -> bool {
        self.tier_of(path).is_some()
    }

    /// The tier of a tree file (`system` or `user`), None if it is not one. (The broker decides;
    /// this is what the shell expects it to say.)
    pub fn tier_of(&self, path: &str) -> Option<&str> {
        self.nodes.values().find(|n| n.file.as_deref().is_some_and(|p| matches_file(p, path).is_some())).map(|n| n.tier.as_str())
    }

    fn node(&self, key: &str) -> &Node {
        &self.nodes[key]
    }

    /// The nodes one word below `key` ("" for the top): (word, node key). Below a list, the one child
    /// is its entry, `<name>`.
    pub fn children(&self, key: &str) -> Vec<(String, String)> {
        let depth = if key.is_empty() { 0 } else { key.split(' ').count() };
        self.nodes
            .keys()
            .filter(|k| k.split(' ').count() == depth + 1 && (key.is_empty() || k.starts_with(&format!("{key} "))))
            .map(|k| (k.rsplit(' ').next().unwrap().to_string(), k.clone()))
            .collect()
    }

    /// Resolve the words of `set …` / `delete …` / `show configuration …` to a node. Stops at a leaf
    /// or leaf-list (the rest are its values) or when the words run out (a container or list).
    pub fn resolve(&self, words: &[String]) -> Result<Target> {
        self.resolve_in(words, None)
    }

    /// The same, also accepting list entries that have settings in `cfg` though they are not (or no
    /// longer) among the list's values — a removed user's folder can still be shown and deleted.
    pub fn resolve_in(&self, words: &[String], cfg: Option<&Config>) -> Result<Target> {
        let (mut key, mut path, mut name) = (String::new(), Vec::new(), None);
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            if !key.is_empty() && self.node(&key).kind == Kind::List {
                let list = self.node(&key);
                let existing = cfg.is_some_and(|c| self.entry_names(&key, c).contains(w));
                check_name(&key, list, w, existing)?;
                key = format!("{key} <name>");
                name = Some(w.clone());
            } else {
                let next = if key.is_empty() { w.clone() } else { format!("{key} {w}") };
                if !self.nodes.contains_key(&next) {
                    let options: Vec<String> = self.children(&key).into_iter().map(|(w, _)| w).collect();
                    let at = if key.is_empty() { "the top".to_string() } else { format!("'{}'", path.join(" ")) };
                    bail!("syntax error: '{w}' is not a setting at {at}; one of: {}", options.join(", "));
                }
                key = next;
            }
            path.push(w.clone());
            i += 1;
            if matches!(self.node(&key).kind, Kind::Leaf | Kind::LeafList) {
                break;
            }
        }
        Ok(Target { node: key, words: path, name, values: words[i..].to_vec() })
    }

    /// The repo file a node's data is in, for a list entry `name`.
    fn file_of(&self, key: &str, name: Option<&str>) -> Option<String> {
        let f = self.node(key).file.as_ref()?;
        Some(match name {
            Some(n) => f.replace("<name>", n),
            None => f.clone(),
        })
    }

    /// Check a value for a leaf or leaf-list element.
    fn check_value(&self, key: &str, value: &str) -> Result<()> {
        let n = self.node(key);
        let what = key.replace("<name>", "…");
        if let Some(values) = &n.values {
            if !values.contains_key(value) {
                let hint = crate::near::hint(value, values.keys().map(String::as_str));
                bail!("'{value}' is not a valid {what}{hint}");
            }
            return Ok(());
        }
        let (ok, form) = match n.ty.as_deref() {
            Some("package") => (package_name_ok(value), "a nixpkgs name: letters, digits, - _ and . between names"),
            Some("flatpak") => (flatpak_id_ok(value), "an application's id, like org.mozilla.firefox"),
            Some("host-name") => (host_name_ok(value), "letters, digits and -, at most 63, not starting or ending with -"),
            Some("user-name") => (user_name_ok(value), "small letters, digits, _ and -, starting with a letter or _"),
            // A value with no closed set and no type: still one plain word.
            _ => (!value.is_empty() && !value.chars().any(|c| c.is_whitespace() || c.is_control()), "one word"),
        };
        if !ok {
            bail!("'{value}' is not a valid {what}: {form}");
        }
        Ok(())
    }

    /// `set` / `delete` need a path: the whole configuration is not one setting.
    fn not_the_top(&self, cmd: &str, t: &Target) -> Result<()> {
        if t.node.is_empty() {
            let options: Vec<String> = self.children("").into_iter().map(|(w, _)| w).collect();
            bail!("{cmd}: incomplete; one of: {}", options.join(", "));
        }
        Ok(())
    }

    /// `set <words>`: give a leaf a value, or add elements to a leaf-list.
    pub fn set(&self, cfg: &mut Config, words: &[String]) -> Result<()> {
        let t = self.resolve(words)?;
        self.not_the_top("set", &t)?;
        let n = self.node(&t.node);
        let file = self.file_of(&t.node, t.name.as_deref());
        match (n.kind, file) {
            (Kind::Leaf, Some(file)) => {
                let [value] = t.values.as_slice() else {
                    bail!("{}: takes exactly one value", t.words.join(" "));
                };
                self.check_value(&t.node, value)?;
                let key = n.key.clone().context("schema: a leaf without a key")?;
                match cfg.files.entry(file.clone()).or_insert_with(|| Data::Object(BTreeMap::new())) {
                    Data::Object(o) => {
                        o.insert(key, value.clone());
                    }
                    Data::List(_) => bail!("{file} holds a list, not settings"),
                }
            }
            (Kind::LeafList, Some(file)) => {
                let values = list_values(&t.values)?;
                if values.is_empty() {
                    bail!("set {}: needs a value", t.words.join(" "));
                }
                for v in &values {
                    self.check_value(&t.node, v)?;
                }
                match cfg.files.entry(file.clone()).or_insert_with(|| Data::List(Vec::new())) {
                    Data::List(l) => {
                        l.extend(values);
                        l.sort();
                        l.dedup();
                    }
                    Data::Object(_) => bail!("{file} holds settings, not a list"),
                }
            }
            _ => {
                let options: Vec<String> = self.children(&t.node).into_iter().map(|(w, _)| w).collect();
                bail!("set {}: incomplete; one of: {}", t.words.join(" "), options.join(", "));
            }
        }
        Ok(())
    }

    /// `delete <words>`: remove a leaf, elements of a leaf-list (all of them without values), or
    /// everything below a container or list entry.
    pub fn delete(&self, cfg: &mut Config, words: &[String]) -> Result<()> {
        let t = self.resolve_in(words, Some(cfg))?;
        self.not_the_top("delete", &t)?;
        let n = self.node(&t.node);
        let gone = |what: &str| anyhow::Error::new(NotSet(what.to_string()));
        match n.kind {
            Kind::Leaf => {
                if !t.values.is_empty() {
                    bail!("{}: takes no value here (delete removes it whole)", t.words.join(" "));
                }
                let file = self.file_of(&t.node, t.name.as_deref()).context("schema: a leaf without a file")?;
                let key = n.key.as_deref().context("schema: a leaf without a key")?;
                let Some(Data::Object(o)) = cfg.files.get_mut(&file) else { return Err(gone(key)) };
                o.remove(key).ok_or_else(|| gone(key))?;
                if o.is_empty() {
                    cfg.files.remove(&file);
                }
            }
            Kind::LeafList => {
                let file = self.file_of(&t.node, t.name.as_deref()).context("schema: a leaf-list without a file")?;
                let values: BTreeSet<String> = list_values(&t.values)?.into_iter().collect();
                if values.is_empty() && !t.values.is_empty() {
                    bail!("delete {}: needs a value inside [ ]", t.words.join(" "));
                }
                let Some(Data::List(l)) = cfg.files.get_mut(&file) else { return Err(gone("it")) };
                if values.is_empty() {
                    cfg.files.remove(&file);
                } else {
                    // All or nothing: a failed delete changes nothing.
                    if let Some(missing) = values.iter().find(|v| !l.contains(v)) {
                        return Err(gone(missing));
                    }
                    l.retain(|x| !values.contains(x));
                    if l.is_empty() {
                        cfg.files.remove(&file);
                    }
                }
            }
            Kind::Container | Kind::List => {
                if !t.values.is_empty() {
                    bail!("delete {}: unexpected '{}'", t.words.join(" "), t.values[0]);
                }
                let below: Vec<(String, Option<String>)> = self
                    .instances(cfg)
                    .into_iter()
                    .filter(|(key, name, _)| under(key, name.as_deref(), &t))
                    .map(|(key, name, _)| (key, name))
                    .collect();
                if below.is_empty() {
                    return Err(gone("anything below it"));
                }
                for (key, name) in below {
                    let file = self.file_of(&key, name.as_deref()).expect("instances have files");
                    match (self.node(&key).kind, cfg.files.get_mut(&file)) {
                        (Kind::Leaf, Some(Data::Object(o))) => {
                            o.remove(self.node(&key).key.as_deref().unwrap_or_default());
                            if o.is_empty() {
                                cfg.files.remove(&file);
                            }
                        }
                        _ => {
                            cfg.files.remove(&file);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Read the tree's files (path, content) into a `Config`. Files the tree does not describe are
    /// left out (they are Nix, or the modules' business). A tree file must be exactly what its nodes
    /// make: a JSON object of the nodes' keys with string values, or a JSON list of strings. Anything
    /// else is refused, naming the file — the shell must not hide, or rewrite, what it cannot show.
    pub fn load<'a>(&self, files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Config> {
        let mut cfg = Config::default();
        for (path, bytes) in files {
            let nodes: Vec<(&String, &Node)> =
                self.nodes.iter().filter(|(_, n)| n.file.as_deref().is_some_and(|p| matches_file(p, path).is_some())).collect();
            let Some((first_key, first)) = nodes.first() else { continue };
            let bad = |why: String| anyhow::anyhow!("{path}: {why}; torqcli cannot show it — fix the file (as raw JSON) first");
            // The list entry it belongs to must have a valid name (an existing one: it has settings).
            if let Some(name) = matches_file(first.file.as_deref().unwrap_or_default(), path).flatten() {
                let list = first_key.split(" <name>").next().unwrap_or_default();
                check_name(list, self.node(list), &name, true).map_err(|e| bad(e.to_string()))?;
            }
            // Values as the shell would accept them in form: never whitespace or control characters.
            // (Not membership of a closed set: a zone a newer tzdata dropped must not make an older
            // configuration unreadable; evaluation decides.)
            let value_ok = |key: &str, v: &str| -> Result<()> {
                let n = self.node(key);
                let shaped = match n.ty.as_deref() {
                    Some("package") => package_name_ok(v),
                    Some("flatpak") => flatpak_id_ok(v),
                    Some("host-name") => host_name_ok(v),
                    Some("user-name") => user_name_ok(v),
                    _ => !v.is_empty() && !v.chars().any(|c| c.is_whitespace() || c.is_control()),
                };
                if !shaped {
                    bail!(bad(format!("{} is not a valid {}", serde_json::to_string(v)?, key.replace("<name>", "…"))));
                }
                Ok(())
            };
            let v: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| bad(format!("not JSON ({e})")))?;
            let data = if first.kind == Kind::LeafList {
                let list = v.as_array().ok_or_else(|| bad("must be a JSON list".into()))?;
                let items: Vec<String> = list.iter().map(|x| x.as_str().map(String::from).ok_or_else(|| bad(format!("{x} is not a string")))).collect::<Result<_>>()?;
                for i in &items {
                    value_ok(first_key, i)?;
                }
                // A list's order and repeats carry no meaning (the modules read it as a set): in
                // normal form, a reordered file is the same configuration, not a change.
                let mut items = items;
                items.sort();
                items.dedup();
                Data::List(items)
            } else {
                let obj = v.as_object().ok_or_else(|| bad("must be a JSON object".into()))?;
                let mut o = BTreeMap::new();
                for (k, x) in obj {
                    let Some((key, _)) = nodes.iter().find(|(_, n)| n.key.as_deref() == Some(k.as_str())) else {
                        bail!(bad(format!("\"{k}\" is not a setting")));
                    };
                    let x = x.as_str().ok_or_else(|| bad(format!("\"{k}\" is not a string")))?;
                    value_ok(key, x)?;
                    o.insert(k.clone(), x.to_string());
                }
                Data::Object(o)
            };
            // An empty file is no setting: the same as no file.
            let empty = matches!(&data, Data::List(l) if l.is_empty()) || matches!(&data, Data::Object(o) if o.is_empty());
            if !empty {
                cfg.files.insert(path.to_string(), data);
            }
        }
        Ok(cfg)
    }

    /// The entry names of list `list_key` that have settings in `cfg`.
    fn entry_names(&self, list_key: &str, cfg: &Config) -> BTreeSet<String> {
        let prefix = format!("{list_key} <name>");
        self.instances(cfg)
            .into_iter()
            .filter(|(key, _, _)| key == &prefix || key.starts_with(&format!("{prefix} ")))
            .filter_map(|(_, name, _)| name)
            .collect()
    }

    /// Every set leaf and leaf-list in `cfg`: (node key, list entry name, value).
    fn instances(&self, cfg: &Config) -> Vec<(String, Option<String>, Val)> {
        let mut out = Vec::new();
        for (key, n) in &self.nodes {
            let Some(pattern) = &n.file else { continue };
            for (file, data) in &cfg.files {
                let Some(name) = matches_file(pattern, file) else { continue };
                let val = match (n.kind, data) {
                    (Kind::Leaf, Data::Object(o)) => match o.get(n.key.as_deref().unwrap_or_default()) {
                        Some(v) => Val::One(v.clone()),
                        None => continue,
                    },
                    (Kind::LeafList, Data::List(l)) if !l.is_empty() => {
                        let mut l = l.clone();
                        l.sort();
                        l.dedup();
                        Val::Many(l)
                    }
                    _ => continue,
                };
                out.push((key.clone(), name, val));
            }
        }
        out
    }

    /// The tree's entries as (words, value), in display order.
    pub fn entries(&self, cfg: &Config) -> Vec<(Vec<String>, Val)> {
        let mut out: Vec<(Vec<String>, Val)> = self
            .instances(cfg)
            .into_iter()
            .map(|(key, name, val)| {
                let words = key
                    .split(' ')
                    .map(|w| if w == "<name>" { name.clone().unwrap_or_default() } else { w.to_string() })
                    .collect();
                (words, val)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// `show configuration [<words>]`, J style: containers in braces, `leaf value;`,
    /// `leaf-list [ a b ];`.
    /// The path is resolved first: an unknown setting or name is an error, not empty output; a path
    /// to a leaf or leaf-list shows that one statement.
    #[cfg(test)] // torqcli shows the whole tree (whole.rs); these show the hand-made part alone
    pub fn show(&self, cfg: &Config, prefix: &[String]) -> Result<String> {
        let t = self.resolve_in(prefix, Some(cfg))?;
        if !t.values.is_empty() {
            bail!("show configuration {}: unexpected '{}'", t.words.join(" "), t.values[0]);
        }
        let prefix = &t.words;
        let mut out = String::new();
        // (The top, "", is no node: it is the whole tree.)
        if !t.node.is_empty() && matches!(self.node(&t.node).kind, Kind::Leaf | Kind::LeafList) {
            if let Some((_, v)) = self.entries(cfg).into_iter().find(|(w, _)| w == prefix) {
                out.push_str(&format!("{};\n", statement(prefix.last().unwrap(), &v)));
            }
            return Ok(out);
        }
        let entries: Vec<(Vec<String>, Val)> = self
            .entries(cfg)
            .into_iter()
            .filter(|(w, _)| w.starts_with(prefix))
            .map(|(w, v)| (w[prefix.len()..].to_vec(), v))
            .collect();
        render(&entries, 0, &mut out);
        Ok(out)
    }

    /// The files behind what `show <prefix>` shows (`| display detail`): each config repo file, and
    /// the settings it holds there (those set, not every one it could hold).
    pub fn files_of(&self, cfg: &Config, prefix: &[String]) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        for (path, data) in &cfg.files {
            let mut settings = Vec::new();
            for (key, node) in &self.nodes {
                let Some(name) = node.file.as_deref().and_then(|p| matches_file(p, path)) else { continue };
                let set = match data {
                    Data::Object(o) => node.key.as_deref().is_some_and(|k| o.contains_key(k)),
                    Data::List(l) => !l.is_empty(),
                };
                if !set {
                    continue;
                }
                let words: Vec<String> =
                    key.split(' ').map(|k| if k == "<name>" { name.clone().unwrap_or_default() } else { k.to_string() }).collect();
                if words.starts_with(prefix) || prefix.starts_with(&words) {
                    settings.push(words.join(" "));
                }
            }
            if !settings.is_empty() {
                out.push((path.clone(), settings));
            }
        }
        out
    }

    /// `show configuration | display set`: one `set` command per value.
    #[cfg(test)] // torqcli shows the whole tree (whole.rs); these show the hand-made part alone
    pub fn display_set(&self, cfg: &Config) -> String {
        display_set_entries(&self.entries(cfg))
    }

    /// `show | compare`: what `new` changes from `old`, J style — under `[edit <container>]`,
    /// `-` lines for what goes and `+` lines for what comes.
    #[cfg(test)] // torqcli shows the whole tree (whole.rs); these show the hand-made part alone
    pub fn compare(&self, old: &Config, new: &Config) -> String {
        compare_entries(self.entries(old), self.entries(new))
    }
}

/// `| display set` of any entries (path, value): one `set` command per value.
pub fn display_set_entries(entries: &[(Vec<String>, Val)]) -> String {
    let mut out = String::new();
    for (words, val) in entries {
        let path = words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
        match val {
            Val::One(v) => out.push_str(&format!("set {path} {}\n", quote(v))),
            // An empty list too, so `load set` gives it back.
            Val::Many(vs) if vs.is_empty() => out.push_str(&format!("set {path} [ ]\n")),
            Val::Many(vs) => {
                for v in vs {
                    out.push_str(&format!("set {path} {}\n", quote(v)));
                }
            }
        }
    }
    out
}

/// `show | compare` of any two sets of entries: what `new` changes from `old`.
pub fn compare_entries(old: Vec<(Vec<String>, Val)>, new: Vec<(Vec<String>, Val)>) -> String {
    let (a, b): (BTreeMap<_, _>, BTreeMap<_, _>) = (old.into_iter().collect(), new.into_iter().collect());
    // By section, then setting: a section's own settings together, before its sub-sections, so each
    // `[edit …]` is said once (as on a network device).
    let paths: BTreeSet<(&[String], &Vec<String>)> = a.keys().chain(b.keys()).map(|p| (&p[..p.len() - 1], p)).collect();
    let paths = paths.into_iter().map(|(_, p)| p);
    let mut out = String::new();
    let mut section: Option<Vec<String>> = None;
    for p in paths {
        let (was, now) = (a.get(p), b.get(p));
        if was == now {
            continue;
        }
        let parent = p[..p.len() - 1].to_vec();
        if section.as_ref() != Some(&parent) {
            // (Each word as it is typed: a name with spaces quoted, so the section reads back as a path.)
            out.push_str(&if parent.is_empty() { "[edit]\n".to_string() } else { format!("[edit {}]\n", parent.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")) });
            section = Some(parent);
        }
        let leaf = p.last().unwrap();
        if let Some(v) = was {
            out.push_str(&format!("-   {};\n", statement(leaf, v)));
        }
        if let Some(v) = now {
            out.push_str(&format!("+   {};\n", statement(leaf, v)));
        }
    }
    out
}

/// Any entries (path, value), sorted, in the brace format `show configuration` prints.
pub fn render_entries(entries: &[(Vec<String>, Val)]) -> String {
    let mut out = String::new();
    render(entries, 0, &mut out);
    out
}

/// `name` names a list entry of `list` (at `key`): its type, and one of its values if it has them
/// (unless the entry `existing`ly has settings).
fn check_name(key: &str, list: &Node, name: &str, existing: bool) -> Result<()> {
    let shaped = match list.ty.as_deref() {
        Some("user-name") => user_name_ok(name),
        // Any other list: a plain word, never a path or an option.
        _ => {
            let mut c = name.chars();
            c.next().is_some_and(|f| f.is_ascii_alphanumeric() || f == '_')
                && c.all(|x| x.is_ascii_alphanumeric() || "_.-".contains(x))
        }
    };
    if !shaped {
        bail!("'{name}' is not a valid name in {key}");
    }
    // `all` as an entry name would be ambiguous (`delete users all`).
    if crate::syntax::RESERVED.contains(&name) {
        bail!("'{name}' cannot be named in {key}: torqcli keeps the word for a command");
    }
    if existing {
        return Ok(());
    }
    if let Some(values) = &list.values {
        if !values.contains_key(name) {
            let known = values.keys().cloned().collect::<Vec<_>>().join(", ");
            // How a new one is made, where the list says what its names are (a person: declared in
            // the candidate, counted in the same one — whole.rs).
            let new = match list.ty.as_deref() {
                Some("user-name") => format!("; a new person first: `set users users {name} isNormalUser true`"),
                _ => String::new(),
            };
            bail!("'{name}' is not one of the {key} on this machine ({known}){new}");
        }
    }
    Ok(())
}

/// The list entry name if `file` is an instance of the schema's file `pattern` (`users/<name>/x`).
fn matches_file(pattern: &str, file: &str) -> Option<Option<String>> {
    match pattern.split_once("<name>") {
        None => (pattern == file).then_some(None),
        Some((pre, post)) => {
            let name = file.strip_prefix(pre)?.strip_suffix(post)?;
            (!name.is_empty() && !name.contains('/')).then(|| Some(name.to_string()))
        }
    }
}

/// Whether node `key` (for list entry `name`) lies below the target's container or list entry.
fn under(key: &str, name: Option<&str>, t: &Target) -> bool {
    let inside = key.starts_with(&format!("{} ", t.node));
    inside && (t.name.is_none() || name == t.name.as_deref())
}

/// A leaf-list's values: `a b` or `[ a b ]`.
fn list_values(words: &[String]) -> Result<Vec<String>> {
    match words.first().map(String::as_str) {
        Some("[") => {
            if words.last().map(String::as_str) != Some("]") || words.len() < 2 {
                bail!("missing ']'");
            }
            Ok(words[1..words.len() - 1].to_vec())
        }
        _ => Ok(words.to_vec()),
    }
}

fn statement(leaf: &str, v: &Val) -> String {
    match v {
        // The setting's own name quoted too: what `show` prints, a saved file reads back (saved.rs).
        Val::One(v) => format!("{} {}", quote(leaf), quote(v)),
        Val::Many(vs) => format!("{} [ {} ]", quote(leaf), vs.iter().map(|v| quote(v)).collect::<Vec<_>>().join(" ")),
    }
}

fn render(entries: &[(Vec<String>, Val)], depth: usize, out: &mut String) {
    let indent = "    ".repeat(depth);
    let mut i = 0;
    while i < entries.len() {
        let (words, val) = &entries[i];
        if words.len() == 1 {
            out.push_str(&format!("{indent}{};\n", statement(&words[0], val)));
            i += 1;
            continue;
        }
        let head = &words[0];
        let j = entries[i..].iter().position(|(w, _)| w.len() == 1 || &w[0] != head).map_or(entries.len(), |p| i + p);
        let group: Vec<(Vec<String>, Val)> = entries[i..j].iter().map(|(w, v)| (w[1..].to_vec(), v.clone())).collect();
        out.push_str(&format!("{indent}{} {{\n", quote(head)));
        render(&group, depth + 1, out);
        out.push_str(&format!("{indent}}}\n"));
        i = j;
    }
}

/// A word as J style shows it: quoted if it has spaces or characters the syntax uses.
fn quote(w: &str) -> String {
    // (`|` too: a line torqcli reads splits there.)
    if w.is_empty() || w.chars().any(|c| c.is_whitespace() || c.is_control() || ";{}[]\"#'\\|".contains(c)) {
        format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        w.to_string()
    }
}

/// The form of a package name, as the broker and modules/packages.nix accept it: a top-level
/// nixpkgs attribute, or one level under a package set.
pub fn package_name_ok(name: &str) -> bool {
    let segment = |s: &str| {
        let mut c = s.chars();
        c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|x| x.is_ascii_alphanumeric() || "_'+-".contains(x))
    };
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2 && parts.iter().all(|p| segment(p))
}

/// RFC 1123, one label (modules/host-rules.nix).
pub fn host_name_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// A user's login name as the broker treats it as user tier: `[a-z_][a-z0-9_-]*`.
pub fn user_name_ok(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_lowercase() || f == '_')
        && c.all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == '_' || x == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real schema, as the modules produce it for a machine whose one user is `dev`
    /// (tests/tree.nix checks this copy is current). Regenerate after a schema change:
    ///   nix eval --impure --raw --expr 'let f = builtins.getFlake (toString ./.); in (f.lib.mkSystem {
    ///     system = "aarch64-linux"; configDir = ./tests/tree/cli-good; modules = [ { torq.user = "dev"; } ];
    ///   }).config.environment.etc."torq/schema.json".text' | jq -S . > cli/src/schema-fixture.json
    fn schema() -> Schema {
        Schema::parse(include_str!("schema-fixture.json")).unwrap()
    }
    fn w(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn err(r: Result<()>) -> String {
        format!("{:#}", r.unwrap_err())
    }
    fn configured() -> Config {
        let (s, mut c) = (schema(), Config::default());
        for cmd in [
            "system host-name torq",
            "system time-zone Europe/Berlin",
            "system packages [ ripgrep htop ]",
            "services ssh",
            "users dev packages jq",
        ] {
            s.set(&mut c, &w(cmd)).unwrap();
        }
        c
    }

    #[test]
    fn resolves_paths_through_the_schema() {
        let s = schema();
        let t = s.resolve(&w("users dev packages jq")).unwrap();
        assert_eq!((t.node.as_str(), t.name.as_deref(), t.values.clone()), ("users <name> packages", Some("dev"), w("jq")));
        assert_eq!(t.words, w("users dev packages"));
        let t = s.resolve(&w("system")).unwrap();
        assert_eq!((t.node.as_str(), t.values.len()), ("system", 0));
        let e = format!("{:#}", s.resolve(&w("system hostname x")).unwrap_err());
        assert!(e.contains("'hostname' is not a setting at 'system'") && e.contains("host-name"), "{e}");
        // List entries: only the machine's users, and only well-formed names, before any path is built.
        for (bad, why) in [("nobody", "not one of the users"), ("Bad.Name", "not a valid name"), ("../etc", "not a valid name"), ("cli", "not one of the users")] {
            let e = format!("{:#}", s.resolve(&w(&format!("users {bad} packages jq"))).unwrap_err());
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn set_writes_one_key_or_list_of_one_file() {
        let c = configured();
        assert_eq!(c.files["system/host.json"], Data::Object([("host-name".into(), "torq".into()), ("time-zone".into(), "Europe/Berlin".into())].into()));
        assert_eq!(c.files["system/packages.json"], Data::List(w("htop ripgrep")));
        assert_eq!(c.files["services.json"], Data::List(w("ssh")));
        assert_eq!(c.files["users/dev/packages.json"], Data::List(w("jq")));
        assert_eq!(c.files.len(), 4);
    }

    #[test]
    fn set_checks_values() {
        let (s, mut c) = (schema(), Config::default());
        let e = err(s.set(&mut c, &w("system time-zone Europe/Berln")));
        assert!(e.contains("not a valid system time-zone"), "{e}");
        let e = err(s.set(&mut c, &w("system time-zone berlin")));
        assert!(e.contains("; did you mean Europe/Berlin?"), "{e}");
        for bad in ["system host-name my_laptop", "system host-name -x", "system packages ../x", "system packages a.b.c", "services telnet"] {
            assert!(err(s.set(&mut c, &w(bad))).contains("is not a valid"), "{bad}");
        }
        assert!(err(s.set(&mut c, &w("system host-name a b"))).contains("exactly one value"));
        assert!(err(s.set(&mut c, &w("system packages"))).contains("needs a value"));
        assert!(err(s.set(&mut c, &w("system packages [ htop"))).contains("missing ']'"));
        let e = err(s.set(&mut c, &w("users dev")));
        assert!(e.contains("incomplete; one of: flatpaks, packages"), "{e}");
        assert_eq!(c, Config::default(), "nothing invalid was written");
    }

    #[test]
    fn delete_removes_exactly_what_it_names() {
        let s = schema();
        let mut c = configured();
        s.delete(&mut c, &w("system time-zone")).unwrap();
        assert_eq!(c.files["system/host.json"], Data::Object([("host-name".into(), "torq".into())].into()));
        s.delete(&mut c, &w("system host-name")).unwrap();
        assert!(!c.files.contains_key("system/host.json"), "an empty object is no file");
        s.delete(&mut c, &w("system packages htop")).unwrap();
        assert_eq!(c.files["system/packages.json"], Data::List(w("ripgrep")));
        s.delete(&mut c, &w("system packages")).unwrap();
        assert!(!c.files.contains_key("system/packages.json"));
        // A list entry: everything below it, each in its own file.
        s.delete(&mut c, &w("users dev")).unwrap();
        assert!(c.files.keys().all(|f| !f.starts_with("users/")), "{:?}", c.files.keys());
        assert_eq!(c.files.keys().collect::<Vec<_>>(), ["services.json"]);
        // Not set: an error ("statement not found", as network devices say).
        for gone in ["system host-name", "system packages", "system packages vim", "users dev", "system"] {
            assert!(err(s.delete(&mut c, &w(gone))).contains("is not set"), "{gone}");
        }
        assert!(err(s.delete(&mut c, &w("system host-name torq"))).contains("takes no value"));
    }

    #[test]
    fn delete_of_a_container_takes_everything_below_it() {
        let (s, mut c) = (schema(), configured());
        s.delete(&mut c, &w("system")).unwrap();
        assert_eq!(c.files.keys().collect::<Vec<_>>(), ["services.json", "users/dev/packages.json"]);
        let mut c = configured();
        s.delete(&mut c, &w("users")).unwrap();
        assert!(c.files.keys().all(|f| !f.starts_with("users/")));
    }

    #[test]
    fn show_configuration_is_j_style() {
        let (s, c) = (schema(), configured());
        assert_eq!(
            s.show(&c, &[]).unwrap(),
            "services [ ssh ];\n\
             system {\n    host-name torq;\n    packages [ htop ripgrep ];\n    time-zone Europe/Berlin;\n}\n\
             users {\n    dev {\n        packages [ jq ];\n    }\n}\n"
        );
        assert_eq!(s.show(&c, &w("users dev")).unwrap(), "packages [ jq ];\n");
        assert_eq!(s.show(&Config::default(), &[]).unwrap(), "");
        // A path to a leaf or leaf-list shows that statement (review: this used to panic).
        assert_eq!(s.show(&c, &w("system host-name")).unwrap(), "host-name torq;\n");
        assert_eq!(s.show(&c, &w("system packages")).unwrap(), "packages [ htop ripgrep ];\n");
        assert_eq!(s.show(&c, &w("users dev packages")).unwrap(), "packages [ jq ];\n");
        assert_eq!(s.show(&Config::default(), &w("system host-name")).unwrap(), "");
        // An unknown setting or name is an error, not empty output.
        assert!(format!("{:#}", s.show(&c, &w("users nobody")).unwrap_err()).contains("not one of the users"));
        assert!(format!("{:#}", s.show(&c, &w("system hostname")).unwrap_err()).contains("not a setting"));
    }

    /// Each section once: its own settings, then its sub-sections.
    #[test]
    fn a_comparison_says_each_section_once() {
        let e = |p: &str, v: &str| (p.split(' ').map(String::from).collect::<Vec<_>>(), Val::One(v.into()));
        let out = compare_entries(vec![], vec![e("networking domain", "x"), e("networking firewall enable", "true"), e("networking nameservers", "y")]);
        assert_eq!(out, "[edit networking]\n+   domain x;\n+   nameservers y;\n[edit networking firewall]\n+   enable true;\n");
    }

    #[test]
    fn an_empty_set_or_delete_is_incomplete() {
        let (s, mut c) = (schema(), configured());
        for r in [s.set(&mut c, &[]), s.delete(&mut c, &[])] {
            assert!(err(r).contains("incomplete; one of: services, system, users"));
        }
    }

    /// A failed delete changes nothing; duplicates and `[ ]` do not slip through (review of 3a).
    #[test]
    fn delete_is_all_or_nothing() {
        let s = schema();
        let mut c = configured();
        let before = c.clone();
        assert!(err(s.delete(&mut c, &w("system packages htop vim"))).contains("vim is not set"));
        assert_eq!(c, before, "htop is still there");
        s.delete(&mut c, &w("system packages htop ripgrep htop")).unwrap();
        assert!(!c.files.contains_key("system/packages.json"), "an emptied list is no file, duplicates or not");
        let mut c = before.clone();
        assert!(err(s.delete(&mut c, &w("system packages [ ]"))).contains("needs a value inside [ ]"));
        assert_eq!(c, before);
        // A list loaded with a duplicate loses every copy.
        let mut c = s.load([("system/packages.json", br#"["htop","htop","vim"]"#.as_slice())]).unwrap();
        s.delete(&mut c, &w("system packages htop")).unwrap();
        assert_eq!(c.files["system/packages.json"], Data::List(w("vim")));
    }

    /// Settings left by a user who is no longer on the machine can be shown and deleted, not set.
    #[test]
    fn a_removed_users_settings_can_be_cleaned_up() {
        let s = schema();
        let mut c = s.load([("users/old/packages.json", br#"["vim"]"#.as_slice())]).unwrap();
        assert_eq!(s.show(&c, &w("users old")).unwrap(), "packages [ vim ];\n");
        assert!(err(s.set(&mut c, &w("users old packages htop"))).contains("not one of the users"));
        s.delete(&mut c, &w("users old")).unwrap();
        assert!(c.files.is_empty());
        // Still only well-formed names: a file under a malformed one does not even load.
        let e = format!("{:#}", s.load([("users/Old/packages.json", br#"["vim"]"#.as_slice())]).unwrap_err());
        assert!(e.contains("users/Old/packages.json") && e.contains("not a valid name"), "{e}");
    }

    /// Loading checks the form of names and values (review of ef8d7f8): nothing the shell could not
    /// show plainly, or read back from `display set`, gets in. Empty files are no settings.
    #[test]
    fn load_checks_names_and_values() {
        let s = schema();
        for (path, body, why) in [
            ("users/a b/packages.json", r#"["vim"]"#, "not a valid name"),
            ("users/dev/packages.json", "[\"x\\ny\"]", "is not a valid users … packages"),
            ("system/packages.json", r#"["a.b.c"]"#, "is not a valid system packages"),
            ("system/host.json", r#"{"host-name":"my_laptop"}"#, "is not a valid system host-name"),
        ] {
            let e = format!("{:#}", s.load([(path, body.as_bytes())]).unwrap_err());
            assert!(e.contains(path) && e.contains(why), "{path}: {e}");
        }
        // A closed set is checked in form only: a zone tzdata no longer lists still loads.
        let c = s.load([("system/host.json", br#"{"time-zone":"Asia/Old_Zone"}"#.as_slice())]).unwrap();
        assert_eq!(s.show(&c, &w("system time-zone")).unwrap(), "time-zone Asia/Old_Zone;\n");
        let c = s.load([("system/host.json", b"{}".as_slice()), ("services.json", b"[]".as_slice())]).unwrap();
        assert!(c.files.is_empty());
    }

    #[test]
    fn no_setting_is_named_like_a_command() {
        let mut v: serde_json::Value = serde_json::from_str(include_str!("schema-fixture.json")).unwrap();
        v["nodes"]["system config"] = serde_json::json!({"kind":"container","help":"x"});
        let e = format!("{:#}", Schema::parse(&v.to_string()).unwrap_err());
        assert!(e.contains("'system config' uses a word torqcli keeps for commands"), "{e}");
    }

    /// The schema itself: two nodes may only share a file as leaves of one object (review of ef8d7f8).
    #[test]
    fn schema_files_do_not_overlap() {
        let base: serde_json::Value = serde_json::from_str(include_str!("schema-fixture.json")).unwrap();
        for (path, node) in [
            ("system packages2", r#"{"kind":"leaf","help":"x","file":"system/packages.json","key":"k"}"#),
            ("users <name> other", r#"{"kind":"leaf-list","help":"x","file":"users/<name>/packages.json"}"#),
            ("system special", r#"{"kind":"leaf-list","help":"x","file":"users/dev/packages.json"}"#),
        ] {
            let mut v = base.clone();
            v["nodes"][path] = serde_json::from_str(node).unwrap();
            let e = format!("{:#}", Schema::parse(&v.to_string()).unwrap_err());
            assert!(e.contains("would share a file"), "{path}: {e}");
        }
    }

    /// Loading refuses what the tree cannot show, naming the file; it skips files outside the tree.
    #[test]
    fn load_takes_exactly_the_tree_files() {
        let s = schema();
        let c = s
            .load([
                ("system/host.json", br#"{"host-name":"a","time-zone":"UTC"}"#.as_slice()),
                ("services.json", br#"["ssh"]"#.as_slice()),
                ("users/dev/packages.json", br#"["jq"]"#.as_slice()),
                ("flake.nix", b"{ outputs = _: { }; }".as_slice()),
                ("users/dev.nix", b"{ }".as_slice()),
            ])
            .unwrap();
        assert_eq!(c.files.keys().collect::<Vec<_>>(), ["services.json", "system/host.json", "users/dev/packages.json"]);
        for (path, body, why) in [
            ("services.json", r#"{"ssh":true}"#, "must be a JSON list"),
            ("services.json", r#"["ssh", 1]"#, "is not a string"),
            ("system/host.json", r#"["torq"]"#, "must be a JSON object"),
            ("system/host.json", r#"{"host-name":"a","domain":"x"}"#, "\"domain\" is not a setting"),
            ("system/host.json", r#"{"host-name":5}"#, "\"host-name\" is not a string"),
            ("users/dev/packages.json", "[", "not JSON"),
        ] {
            let e = format!("{:#}", s.load([(path, body.as_bytes())]).unwrap_err());
            assert!(e.contains(path) && e.contains(why), "{path} {body}: {e}");
        }
    }

    #[test]
    fn quote_marks_what_the_syntax_would_misread() {
        assert_eq!(quote("Europe/Berlin"), "Europe/Berlin");
        for (w, q) in [("xorg'", "\"xorg'\""), ("a b", "\"a b\""), ("a\\b", "\"a\\\\b\""), ("", "\"\""), ("a;b", "\"a;b\"")] {
            assert_eq!(quote(w), q, "{w}");
        }
    }

    #[test]
    fn display_set_is_one_command_per_value() {
        let (s, c) = (schema(), configured());
        assert_eq!(
            s.display_set(&c),
            "set services ssh\nset system host-name torq\nset system packages htop\nset system packages ripgrep\n\
             set system time-zone Europe/Berlin\nset users dev packages jq\n"
        );
        // ...and reading it back gives the same configuration.
        let mut again = Config::default();
        for line in s.display_set(&c).lines() {
            s.set(&mut again, &w(line.strip_prefix("set ").unwrap())).unwrap();
        }
        assert_eq!(again, c);
    }

    #[test]
    fn compare_shows_what_changes_under_its_container() {
        let s = schema();
        let old = configured();
        let mut new = old.clone();
        s.set(&mut new, &w("system host-name laptop")).unwrap();
        s.set(&mut new, &w("system packages vim")).unwrap();
        s.delete(&mut new, &w("users dev packages")).unwrap();
        assert_eq!(
            s.compare(&old, &new),
            "[edit system]\n-   host-name torq;\n+   host-name laptop;\n-   packages [ htop ripgrep ];\n+   packages [ htop ripgrep vim ];\n\
             [edit users dev]\n-   packages [ jq ];\n"
        );
        assert_eq!(s.compare(&old, &old), "");
    }

    #[test]
    fn files_are_written_as_torqos_writes_them() {
        assert_eq!(Data::List(w("ripgrep htop htop")).render(), "[\n  \"htop\",\n  \"ripgrep\"\n]\n");
        assert_eq!(Data::List(vec![]).render(), "[]\n");
        let o = Data::Object([("time-zone".into(), "UTC".into()), ("host-name".into(), "a".into())].into());
        assert_eq!(o.render(), "{\n  \"host-name\": \"a\",\n  \"time-zone\": \"UTC\"\n}\n");
    }

    /// Deleting one user's settings leaves everyone else's alone.
    #[test]
    fn delete_of_a_list_entry_is_only_that_entry() {
        let mut s = schema();
        s.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("ana".into(), String::new());
        let mut c = configured();
        s.set(&mut c, &w("users ana packages vim")).unwrap();
        s.delete(&mut c, &w("users dev")).unwrap();
        let users: Vec<&String> = c.files.keys().filter(|f| f.starts_with("users/")).collect();
        assert_eq!(users, ["users/ana/packages.json"]);
    }

    #[test]
    fn value_rules_match_the_modules() {
        for ok in ["htop", "python3Packages.numpy", "g++", "xorg'"] {
            assert!(package_name_ok(ok), "{ok}");
        }
        for bad in ["", "a.b.c", "1password", ".x", "x.", "a b", "../x"] {
            assert!(!package_name_ok(bad), "{bad}");
        }
        assert!(host_name_ok("torq-1") && host_name_ok(&"a".repeat(63)));
        for bad in ["", "-a", "a-", "a.b", "my_laptop", &"a".repeat(64)] {
            assert!(!host_name_ok(bad), "{bad}");
        }
        assert!(user_name_ok("dev") && user_name_ok("_svc") && !user_name_ok("Dev") && !user_name_ok("a.b"));
    }
}

/// Whether `path` is one of a person's own data files, the only user-tier files
/// (`users/<name>/packages.json`, as the broker's allow-list): the files
/// that can never change how the system runs (SSH, say).
pub fn is_user_data(path: &str) -> bool {
    matches!(path.split('/').collect::<Vec<_>>().as_slice(), ["users", name, "packages.json" | "flatpaks.json"] if !name.is_empty())
}

/// A Flatpak application's id, as the broker takes one (broker/src/policy.rs): at least three dot-separated
/// parts of letters, digits, - and _.
pub fn flatpak_id_ok(id: &str) -> bool {
    let part = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let parts: Vec<&str> = id.split('.').collect();
    id.len() <= 255 && parts.len() >= 3 && parts.iter().all(|p| part(p)) && !id.starts_with('-')
}
