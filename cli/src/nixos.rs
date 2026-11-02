//! The NixOS settings tree (docs/CONFIG-IS-THE-DEVICE.md): every NixOS setting of this system as a
//! path of words — `virtualisation docker enable true` for `virtualisation.docker.enable = true` —
//! read from /etc/torq/nixos-schema.json, which the system generates from its own options
//! (modules/nixos-schema.nix). Nothing here is written per setting, so the tree follows NixOS.
//!
//! Pure: it reads the schema it is given, resolves words to a setting, offers what may come next, and
//! turns a setting's value words into the JSON NixOS will be given. The options' types, when the
//! system is evaluated, stay the authority; the checks here are for early, clear errors.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// A type as the schema writes it: `{ name, elem?, left?, right?, values? }`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Type {
    pub name: String,
    pub elem: Option<Box<Type>>,
    pub left: Option<Box<Type>>,
    pub right: Option<Box<Type>>,
    pub values: Option<Vec<Value>>,
    /// A range's bounds (intBetween, numberBetween).
    #[serde(default)]
    pub range: Option<Vec<i64>>,
    /// What a pattern is, in words (`an IPv4 address`), where TorqOS states it.
    #[serde(default)]
    pub form: Option<String>,
    /// What a set's entries are named after on the machine (`interfaces`), where TorqOS states it.
    #[serde(default)]
    pub names: Option<String>,
}

impl Type {
    /// The kind, without its arguments (`strMatching "^…$"` is a `strMatching`).
    fn kind(&self) -> &str {
        self.name.split_whitespace().next().unwrap_or("")
    }

    /// The values a closed set has (true and false, an enum's), for `?` to list and Tab to
    /// complete; None for an open one.
    pub fn choices(&self) -> Option<Vec<String>> {
        match self.kind() {
            "bool" => Some(vec!["false".into(), "true".into()]),
            "enum" => self.values.as_ref().map(|v| v.iter().map(plain).collect()),
            "nullOr" => self.elem.as_ref().and_then(|e| e.choices()),
            _ => None,
        }
    }

    /// A strMatching type's pattern, as NixOS checks it (the whole value must match).
    fn pattern(&self) -> Option<String> {
        let quoted = self.name.strip_prefix("strMatching ")?;
        let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
        // The type's name writes the pattern as a Nix string: \" \\ \$ are " \ $.
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(e) => out.push(e),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        Some(out)
    }

    /// The lowest and highest a number of this type may be.
    fn bounds(&self) -> Option<(i64, i64)> {
        match (self.kind(), self.range.as_deref()) {
            (_, Some([low, high])) => Some((*low, *high)),
            ("unsignedInt8", _) => Some((0, 255)),
            ("unsignedInt16", _) => Some((0, 65535)),
            ("unsignedInt32", _) => Some((0, 4_294_967_295)),
            ("unsignedInt", _) => Some((0, i64::MAX)),
            ("positiveInt", _) => Some((1, i64::MAX)),
            ("signedInt8", _) => Some((-128, 127)),
            ("signedInt16", _) => Some((-32768, 32767)),
            ("signedInt32", _) => Some((-2_147_483_648, 2_147_483_647)),
            _ => None,
        }
    }

    /// Words for `?`: what a value of this type looks like.
    pub fn describe(&self) -> String {
        match self.kind() {
            "bool" => "true or false".into(),
            "nullOr" => self.elem.as_ref().map_or("a value".into(), |e| e.describe()),
            "listOf" => format!("[ {} … ]", self.elem.as_ref().map_or("value".into(), |e| e.describe())),
            "enum" => format!("one of: {}", self.values.iter().flatten().map(plain).collect::<Vec<_>>().join(", ")),
            "package" => "a package name".into(),
            "attrsOf" | "lazyAttrsOf" => format!("<name> {}", self.elem.as_ref().map_or("value".into(), |e| e.describe())),
            "path" | "pathInStore" => "a path (from /)".into(),
            k if is_int(k) => match self.bounds() {
                Some((low, high)) if high < i64::MAX => format!("a number from {low} to {high}"),
                Some((low, _)) => format!("a number, {low} or more"),
                None => "a number".into(),
            },
            "strMatching" => match (&self.form, self.pattern()) {
                (Some(form), _) => form.clone(),
                (None, Some(p)) => format!("text matching {p}"),
                (None, None) => "text".into(),
            },
            "nonEmptyStr" => "text, not empty".into(),
            k if is_str(k) => "text".into(),
            other => format!("{other} (in a config file)"),
        }
    }
}

fn plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_int(k: &str) -> bool {
    matches!(k, "int" | "intBetween" | "unsignedInt" | "unsignedInt8" | "unsignedInt16" | "unsignedInt32" | "positiveInt" | "signedInt8" | "signedInt16" | "signedInt32")
}

fn is_str(k: &str) -> bool {
    matches!(k, "str" | "nonEmptyStr" | "singleLineStr" | "separatedString" | "strMatching" | "lines" | "commas" | "envVar" | "passwdEntry")
}

/// One entry of the schema: a setting (its type and help), or a section that takes any key (`free`:
/// the type each key's value has — RFC 42's `settings`).
#[derive(Debug, Deserialize)]
struct Entry {
    loc: Vec<String>,
    #[serde(rename = "type")]
    ty: Option<Type>,
    #[serde(default)]
    help: String,
    free: Option<Type>,
}

#[derive(Debug, Deserialize)]
struct File {
    version: u32,
    settings: Vec<Entry>,
    /// What the tree may never change (modules/settings-fence.nix).
    #[serde(default)]
    fence: Vec<Vec<String>>,
    /// A person's password, in NixOS's forms: never in the tree (settings-fence.nix, rule 3).
    #[serde(default)]
    passwords: Vec<Vec<String>>,
    /// This machine's nixpkgs, by name.
    #[serde(default)]
    packages: std::collections::BTreeSet<String>,
}

/// One point of the tree: a setting (with its type and help), what is below it, or both (a set of
/// named entries is a setting whose entries hold more settings).
#[derive(Debug, Default, Clone)]
pub struct Node {
    pub setting: Option<(Type, String)>,
    pub children: BTreeMap<String, Node>,
    /// A section that takes any key besides its own (NixOS passes them on as written): what each
    /// such key is.
    pub free: Option<Box<Node>>,
}

/// Where words lead in the tree.
#[derive(Debug, PartialEq)]
pub struct Resolved<'a> {
    /// The setting's help (NixOS's description, its first paragraph).
    pub help: &'a str,
    /// The setting's path in NixOS, with entry names in place of `<name>`
    /// (`services.httpd.virtualHosts."example.com".forceSSL` → [services, httpd, virtualHosts, example.com, forceSSL]).
    pub loc: Vec<String>,
    pub ty: &'a Type,
    /// The words after the path: the value.
    pub values: Vec<String>,
}

#[derive(Clone)]
pub struct Tree {
    pub root: Node,
    fence: Vec<Vec<String>>,
    passwords: Vec<Vec<String>>,
    /// The packages of this machine's nixpkgs, by name (none known: not checked).
    pub packages: std::collections::BTreeSet<String>,
}

impl Tree {
    pub fn parse(json: &str) -> Result<Tree> {
        let f: File = serde_json::from_str(json).context("reading the NixOS settings schema")?;
        if f.version != 1 {
            bail!("the NixOS settings schema is version {}; this torqcli reads version 1", f.version);
        }
        let mut root = Node::default();
        for e in f.settings {
            let mut n = &mut root;
            for w in &e.loc {
                n = n.children.entry(w.clone()).or_default();
            }
            if let Some(ty) = e.ty {
                n.setting = Some((ty, e.help));
            }
            if let Some(free) = e.free {
                let help = "A key of its own: NixOS passes it on to the program as it is written".to_string();
                n.free = Some(Box::new(Node { setting: Some((free, help)), ..Node::default() }));
            }
        }
        Ok(Tree { root, fence: f.fence, passwords: f.passwords, packages: f.packages })
    }

    /// Whether `loc` (entry names in place of `<name>`) is, or is below, a setting the tree may never
    /// change — as modules/settings-fence.nix decides it (a segment ending in `*` matches entry names
    /// starting with what precedes it). The system's evaluation refuses it anyway; this says so early.
    pub fn fenced(&self, loc: &[String]) -> bool {
        // The tree holds data only: a name starting with `_` is a module directive (`_type`, `_module`).
        loc.iter().any(|s| s.starts_with('_')) || matches_any(&self.fence, loc) || self.password(loc)
    }

    /// Whether `name` is a package of this machine's nixpkgs (`python3Packages.rich`: its set is), and
    /// if not, why — with the names closest to it.
    pub fn package(&self, name: &str) -> Result<()> {
        let first = name.split('.').next().unwrap_or(name);
        if self.packages.is_empty() || self.packages.contains(first) {
            return Ok(());
        }
        let hint = crate::near::hint(name, self.packages.iter().map(String::as_str));
        bail!("'{name}' is not a package in this machine's nixpkgs{hint} (`show packages <word>` searches them)")
    }

    /// Whether `loc` is a person's password (settings-fence.nix, rule 3): never in the tree.
    pub fn password(&self, loc: &[String]) -> bool {
        matches_any(&self.passwords, loc)
    }

    /// Where `words` lead: the path as the tree names it (a unique beginning of a name is that name,
    /// as command words abbreviate; an entry's name is as written), the node there, and the words
    /// after a setting that takes a value (its value).
    fn walk(&self, words: &[String]) -> Result<Walk<'_>> {
        let (mut n, mut loc) = (&self.root, Vec::new());
        for (i, w) in words.iter().enumerate() {
            if let Some((ty, _)) = &n.setting {
                if takes_value(ty) {
                    return Ok(Walk { loc, node: n, values: words[i..].to_vec() });
                }
            }
            match step(n, w) {
                Step::Name(c, name) => {
                    loc.push(name);
                    n = c;
                }
                Step::Entry(e) | Step::Item(e) => {
                    loc.push(w.clone());
                    n = e;
                }
                Step::Many(names) => bail!("'{w}' is ambiguous{}: {}", if loc.is_empty() { String::new() } else { format!(" (after `{}`)", loc.join(" ")) }, names.join(", ")),
                Step::None if n.children.contains_key("*") => bail!("'{w}' is not an entry's number (after `{}`): 1, 2, … — the next number adds one", loc.join(" ")),
                // Another system's path (`system services ssh`): where it is here, before any near word.
                Step::None => match crate::vocabulary::concept_of_path(words) {
                    Some(c) => bail!("{}: no such setting here; it is {}: {}", path_text(&loc, w), c.place, c.how),
                    None => bail!("{}: no such setting{}", path_text(&loc, w), did_you_mean(n, w)),
                },
            }
        }
        Ok(Walk { loc, node: n, values: vec![] })
    }

    /// `words` as a setting and its value: where they lead, which must be a setting that takes a
    /// value (the words after it are that value). A path that ends inside the tree, with no value,
    /// names a place, not a setting.
    pub fn resolve(&self, words: &[String]) -> Result<Resolved<'_>> {
        let w = self.walk(words)?;
        match &w.node.setting {
            Some((ty, help)) if takes_value(ty) => Ok(Resolved { help, loc: w.loc, ty, values: w.values }),
            // A service (or anything with an `enable`): how to turn it on.
            _ if w.node.children.contains_key("enable") => bail!("{0}: not a setting by itself: `set {0} enable true` turns it on (`?` shows its settings)", w.loc.join(" ")),
            _ => bail!("{}: not a setting by itself (`?` shows what is below it)", w.loc.join(" ")),
        }
    }

    /// Where in `words` a list's entry is numbered (`… ipv4 addresses 2 address` → [5]), as far as
    /// the words lead.
    pub fn item_positions(&self, words: &[String]) -> Vec<usize> {
        let (mut n, mut at) = (&self.root, Vec::new());
        for (i, w) in words.iter().enumerate() {
            n = match step(n, w) {
                Step::Name(c, _) | Step::Entry(c) => c,
                Step::Item(c) => {
                    at.push(i);
                    c
                }
                _ => break,
            };
        }
        at
    }

    /// Where in `words` an entry is named (`services httpd virtualHosts example.com` → [3]), as far
    /// as the words lead.
    pub fn entry_positions(&self, words: &[String]) -> Vec<usize> {
        let (mut n, mut at) = (&self.root, Vec::new());
        for (i, w) in words.iter().enumerate() {
            n = match step(n, w) {
                Step::Name(c, _) | Step::Item(c) => c,
                Step::Entry(e) => {
                    at.push(i);
                    e
                }
                _ => break,
            };
        }
        at
    }

    /// The type NixOS gives the place `words` lead to, whether or not it takes a value here (a set of
    /// named entries has one too).
    pub fn type_at(&self, words: &[String]) -> Option<&Type> {
        self.walk(words).ok().filter(|w| w.values.is_empty()).and_then(|w| w.node.setting.as_ref().map(|(t, _)| t))
    }

    /// Where `words` lead, setting or not: the path as the tree names it, the setting there if it
    /// takes a value, and the words after it. `delete` names places as well as settings.
    pub fn locate(&self, words: &[String]) -> Result<(Vec<String>, Option<&Type>, Vec<String>)> {
        let w = self.walk(words)?;
        let ty = w.node.setting.as_ref().map(|(t, _)| t).filter(|t| takes_value(t));
        Ok((w.loc, ty, w.values))
    }

    /// What may come after `words` (for `?` and Tab): the names below, `<name>` where an entry is
    /// named, or, at a setting, its values (a closed set: each, so Tab completes them) or what its
    /// value looks like.
    pub fn next(&self, words: &[String]) -> Vec<(String, String)> {
        let Ok(w) = self.walk(words) else { return vec![] };
        if !w.values.is_empty() {
            return vec![];
        }
        if let Some((ty, help)) = &w.node.setting {
            if takes_value(ty) {
                return match ty.choices() {
                    Some(values) => values.into_iter().map(|v| (v, String::new())).collect(),
                    None => vec![(format!("<{}>", ty.describe()), help.clone())],
                };
            }
        }
        if w.node.children.contains_key("*") {
            return vec![("<n>".into(), "An entry by its number: 1, 2, … (the next number adds one)".into())];
        }
        let mut out: Vec<(String, String)> = w
            .node
            .children
            .iter()
            .map(|(name, c)| {
                let help = c.setting.as_ref().map(|(_, h)| h.clone()).unwrap_or_else(|| section_help(c));
                (name.clone(), help)
            })
            .collect();
        if w.node.free.is_some() {
            out.push(("<key>".into(), "Any other key: NixOS passes it on to the program as it is written".into()));
        }
        out
    }
}

/// Where words lead (Tree::walk).
struct Walk<'a> {
    loc: Vec<String>,
    node: &'a Node,
    values: Vec<String>,
}

/// One word further down the tree from `n`.
enum Step<'a> {
    /// A name below: the node, and the name as the tree has it.
    Name(&'a Node, String),
    /// An entry of a list of them, by its number (1, 2, …).
    Item(&'a Node),
    /// An entry's name, of a set of named ones.
    Entry(&'a Node),
    /// The beginning of several names.
    Many(Vec<String>),
    None,
}

/// What `w` is below `n`: a name (exactly, or a unique beginning of one), or an entry's name where
/// entries are named — as written (an entry is named, never guessed at).
fn step<'a>(n: &'a Node, w: &str) -> Step<'a> {
    if let Some(c) = n.children.get(w).filter(|_| w != "<name>" && w != "*") {
        return Step::Name(c, w.to_string());
    }
    // A list of entries with no names (NixOS's listOf submodule): each by its number, from 1.
    if let Some(item) = n.children.get("*") {
        return if w.parse::<usize>().is_ok_and(|i| i >= 1) { Step::Item(item) } else { Step::None };
    }
    if let Some(e) = n.children.get("<name>") {
        return if w == "<name>" { Step::None } else { Step::Entry(e) };
    }
    // A section that takes any key: a key of its own, never guessed at (as an entry's name).
    if let Some(f) = &n.free {
        return if w.starts_with('<') { Step::None } else { Step::Name(f, w.to_string()) };
    }
    // The same name in another case (`hostname`: hostName), then a unique beginning — as typed, then
    // in any case.
    let lw = w.to_lowercase();
    let same: Vec<(&String, &Node)> = n.children.iter().filter(|(k, _)| k.to_lowercase() == lw).collect();
    if let [(name, c)] = same.as_slice() {
        return Step::Name(c, name.to_string());
    }
    let mut begins: Vec<(&String, &Node)> = n.children.iter().filter(|(k, _)| !w.is_empty() && k.starts_with(w)).collect();
    if begins.is_empty() {
        begins = n.children.iter().filter(|(k, _)| !w.is_empty() && k.to_lowercase().starts_with(&lw)).collect();
    }
    match begins.as_slice() {
        [(name, c)] => Step::Name(c, name.to_string()),
        [] => Step::None,
        many => Step::Many(many.iter().map(|(k, _)| k.to_string()).collect()),
    }
}

/// What a section is, where NixOS describes only its settings: what it holds.
pub fn section_help(n: &Node) -> String {
    const SHOWN: usize = 5;
    let names: Vec<&str> = n.children.keys().map(String::as_str).filter(|k| *k != "<name>" && !k.starts_with('_')).collect();
    match names.len() {
        0 if n.children.contains_key("<name>") => "Named entries, each with its settings".into(),
        0 => String::new(),
        k if k <= SHOWN => format!("Settings: {}", names.join(", ")),
        k => format!("Settings: {}, and {} more", names[..SHOWN].join(", "), k - SHOWN),
    }
}

/// Whether `loc` is, or is below, one of `pats` — as settings-fence.nix matches them (a segment ending
/// in `*` matches entry names starting with what precedes it).
fn matches_any(pats: &[Vec<String>], loc: &[String]) -> bool {
    let seg = |pat: &String, s: &String| match pat.strip_suffix('*') {
        Some(prefix) => s.starts_with(prefix),
        None => pat == s,
    };
    pats.iter().any(|pat| loc.len() >= pat.len() && pat.iter().zip(loc).all(|(p, s)| seg(p, s)))
}

/// The package names in `v`, a value of type `ty` (what `Tree::package` checks).
pub fn packages_in<'v>(ty: &Type, v: &'v Value) -> Vec<&'v str> {
    match (ty.kind(), v) {
        ("package", Value::String(s)) => vec![s.as_str()],
        ("listOf", Value::Array(items)) => ty.elem.as_deref().map(|e| items.iter().flat_map(|i| packages_in(e, i)).collect()).unwrap_or_default(),
        ("nullOr", _) => ty.elem.as_deref().map(|e| packages_in(e, v)).unwrap_or_default(),
        ("attrsOf" | "lazyAttrsOf", Value::Object(o)) => ty.elem.as_deref().map(|e| o.values().flat_map(|x| packages_in(e, x)).collect()).unwrap_or_default(),
        ("either", _) => [ty.left.as_deref(), ty.right.as_deref()].into_iter().flatten().flat_map(|t| packages_in(t, v)).collect(),
        _ => vec![],
    }
}

/// Whether a setting takes its value right here (rather than holding settings below it): everything
/// but a set or list of submodules, a submodule itself, and what only Nix code can say.
fn takes_value(t: &Type) -> bool {
    match t.kind() {
        "submodule" => false,
        "attrsOf" | "lazyAttrsOf" | "listOf" | "nullOr" => t.elem.as_ref().is_some_and(|e| takes_value(e)),
        _ => true,
    }
}

fn path_text(loc: &[String], w: &str) -> String {
    let mut p = loc.to_vec();
    p.push(w.to_string());
    p.join(" ")
}

/// A close name at this point, for a misspelling (`virtualisation dokcer` → docker).
fn did_you_mean(n: &Node, w: &str) -> String {
    crate::near::hint(w, n.children.keys().map(String::as_str))
}

/// A setting's value words as the JSON NixOS will get, checked against its type: `true`/`false`
/// (no words: true), numbers, text, one of an enum's values, `[ a b ]` for a list, package names,
/// and for a set of plain values, `<name> <value>` (`environment variables EDITOR vim`). What only
/// Nix can say (functions, free-form sets) is refused: it belongs in a config file.
pub fn value(ty: &Type, words: &[String]) -> Result<Value> {
    let one = |what: &str| -> Result<&String> {
        match words {
            [w] => Ok(w),
            [] => bail!("needs {what}"),
            _ => bail!("takes one value ({what}); in \"quotes\" if it has spaces"),
        }
    };
    Ok(match ty.kind() {
        "bool" => match words {
            [] => Value::Bool(true),
            [w] if w == "true" => Value::Bool(true),
            [w] if w == "false" => Value::Bool(false),
            _ => bail!("true or false"),
        },
        "nullOr" => value(ty.elem.as_deref().context("a nullOr without its type")?, words)?,
        "enum" => {
            let w = one("one of its values")?;
            let ok = ty.values.iter().flatten().find(|v| plain(v) == *w).cloned();
            ok.with_context(|| format!("'{w}' is not one of: {}", ty.values.iter().flatten().map(plain).collect::<Vec<_>>().join(", ")))?
        }
        "listOf" => {
            let elem = ty.elem.as_deref().context("a list without its type")?;
            let items: &[String] = match words {
                [first, inner @ .., last] if first == "[" && last == "]" => inner,
                [w] if w == "[" || w == "]" => bail!("a list is [ a b … ]"),
                other => other,
            };
            Value::Array(items.iter().map(|i| value(elem, std::slice::from_ref(i))).collect::<Result<_>>()?)
        }
        "attrsOf" | "lazyAttrsOf" => {
            let elem = ty.elem.as_deref().context("a set without its type")?;
            match words {
                [name, rest @ ..] if !rest.is_empty() => {
                    Value::Object([(name.clone(), value(elem, rest)?)].into_iter().collect())
                }
                _ => bail!("<name> <value>"),
            }
        }
        "either" => {
            let (l, r) = (ty.left.as_deref().context("either")?, ty.right.as_deref().context("either")?);
            match value(l, words) {
                Ok(v) => v,
                Err(_) => value(r, words)?,
            }
        }
        k if is_int(k) => {
            let w = one("a number")?;
            let n: i64 = w.parse().map_err(|_| anyhow::anyhow!("'{w}' is not a number"))?;
            if let Some((low, high)) = ty.bounds().filter(|(l, h)| !(*l..=*h).contains(&n)) {
                if high == i64::MAX {
                    bail!("{n} is out of range: {low} or more");
                }
                bail!("{n} is out of range: {low} to {high}");
            }
            Value::from(n)
        }
        "package" => {
            let w = one("a package name")?;
            if !crate::tree::package_name_ok(w) {
                bail!("'{w}' is not a package name");
            }
            Value::String(w.clone())
        }
        "path" | "pathInStore" => {
            let w = one("a path")?;
            if !w.starts_with('/') {
                bail!("'{w}' is not a path: a path starts at / (/srv/www, not srv/www)");
            }
            Value::String(w.clone())
        }
        "strMatching" => {
            let w = one("text")?;
            let p = ty.pattern().context("a strMatching type without its pattern")?;
            // As NixOS checks it (builtins.match): the whole value matches.
            let re = regex::Regex::new(&format!("^(?:{p})$")).with_context(|| format!("this setting's pattern ({p}) is not one torqcli can check: put it in a config file"))?;
            if !re.is_match(w) {
                match &ty.form {
                    Some(form) => bail!("'{w}' is not {form}"),
                    None => bail!("'{w}' is not in the form this setting takes (text matching {p})"),
                }
            }
            Value::String(w.clone())
        }
        "nonEmptyStr" if one("text")?.trim().is_empty() => bail!("this setting takes text that is not empty"),
        "passwdEntry" if one("text")?.contains([':', '\n']) => bail!("'{}' has a ':' (or a line break): an account's fields may not", one("text")?),
        k if is_str(k) => Value::String(one("text")?.clone()),
        other => bail!("a {other} is not a value here: put it in a config file"),
    })
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
    fn set(t: &Tree, line: &str) -> Result<(Vec<String>, Value)> {
        let r = t.resolve(&w(line))?;
        Ok((r.loc.clone(), value(r.ty, &r.values)?))
    }

    #[test]
    fn nixos_paths_one_to_one() {
        let t = tree();
        assert_eq!(set(&t, "virtualisation docker enable true").unwrap(), (w("virtualisation docker enable"), Value::Bool(true)));
        assert_eq!(set(&t, "virtualisation docker enable").unwrap().1, Value::Bool(true), "a bare switch is on");
        assert_eq!(set(&t, "services httpd virtualHosts example.com forceSSL true").unwrap(), (w("services httpd virtualHosts example.com forceSSL"), Value::Bool(true)));
        assert_eq!(set(&t, "services httpd virtualHosts example.com documentRoot /var/www").unwrap().1, Value::from("/var/www"));
        assert_eq!(set(&t, "networking firewall allowedTCPPorts [ 80 443 ]").unwrap().1, serde_json::json!([80, 443]));
        assert_eq!(set(&t, "environment systemPackages [ htop git ]").unwrap().1, serde_json::json!(["htop", "git"]));
        assert_eq!(set(&t, "services openssh settings LogLevel VERBOSE").unwrap().1, Value::from("VERBOSE"));
        assert_eq!(set(&t, "services openssh settings PasswordAuthentication false").unwrap().1, Value::Bool(false));
        assert_eq!(set(&t, "time timeZone Europe/Berlin").unwrap().1, Value::from("Europe/Berlin"));
        assert_eq!(set(&t, "environment variables EDITOR vim").unwrap(), (w("environment variables"), serde_json::json!({"EDITOR": "vim"})));
        assert_eq!(set(&t, "torq dev enable false").unwrap().1, Value::Bool(false), "TorqOS's own settings are there too");
    }

    #[test]
    fn values_are_checked_against_nixos_types() {
        let t = tree();
        for (line, want) in [
            ("virtualisation docker enable yes", "true or false"),
            ("networking firewall allowedTCPPorts [ 80 70000 ]", "0 to 65535"),
            ("networking firewall allowedTCPPorts [ http ]", "not a number"),
            ("services openssh settings LogLevel LOUD", "'LOUD' is not one of: QUIET"),
            ("environment systemPackages [ \"rm -rf\" ]", "not a package name"),
            ("time timeZone", "needs text"),
            ("virtualisation dokcer enable", "no such setting; did you mean docker?"),
            ("system services ssh", "system services: no such setting here; it is services openssh: `set services openssh enable true`"),
            ("interfaces ge-0/0/0 unit 0", "interfaces: no such setting here; it is networking interfaces:"),
            ("services httpd virtualHosts", "not a setting by itself"),
            ("boot kernel sysctl", "not a setting by itself"),
        ] {
            let e = format!("{:#}", set(&t, line).unwrap_err());
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    /// A setting's rules come from NixOS's own description of it: closed sets list their values (so
    /// Tab completes them), and `set` refuses what NixOS would — a pattern, a range, a path — saying
    /// the form the setting takes.
    #[test]
    fn a_value_is_checked_as_nixos_checks_it() {
        let t = tree();
        let w = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let next = |s: &str| t.next(&w(s)).into_iter().map(|(x, _)| x).collect::<Vec<_>>();
        assert_eq!(next("services httpd enable"), ["false", "true"], "a closed set: each value");
        let host = t.resolve(&w("networking hostName")).unwrap().ty.clone();
        assert!(value(&host, &w("my-box")).is_ok() && value(&host, &[String::new()]).is_ok(), "^$ is allowed: empty asks DHCP");
        let e = format!("{:#}", value(&host, &w("my_box!")).unwrap_err());
        assert!(e.starts_with("'my_box!' is not in the form this setting takes (text matching ^$|^[[:alnum:]]"), "{e}");
        let ty = |name: &str, range: Option<Vec<i64>>| Type { name: name.into(), elem: None, left: None, right: None, values: None, range, form: None, names: None };
        let pct = ty("intBetween", Some(vec![1, 100]));
        assert!(value(&pct, &w("100")).is_ok());
        assert_eq!(format!("{:#}", value(&pct, &w("101")).unwrap_err()), "101 is out of range: 1 to 100");
        assert_eq!(pct.describe(), "a number from 1 to 100");
        assert_eq!(format!("{:#}", value(&ty("positiveInt", None), &w("0")).unwrap_err()), "0 is out of range: 1 or more");
        assert!(format!("{:#}", value(&ty("path", None), &w("srv/www")).unwrap_err()).contains("a path starts at /"));
        assert!(value(&ty("nonEmptyStr", None), &[" ".to_string()]).is_err());
        assert!(value(&ty("passwdEntry", None), &w("a:b")).is_err());
        let date = ty("strMatching \"[[:digit:]]{4}-[[:digit:]]{2}-[[:digit:]]{2}\"", None);
        assert!(value(&date, &w("2027-01-31")).is_ok() && value(&date, &w("tomorrow")).is_err() && value(&date, &w("2027-01-31x")).is_err(), "the whole value matches");
        // A form TorqOS states (an address, which NixOS takes as any text): said in words.
        let v4 = t.resolve(&w("networking interfaces eth0 ipv4 addresses 1 address")).unwrap().ty.clone();
        assert!(value(&v4, &w("192.168.64.10")).is_ok());
        for bad in ["999.1.1.1", "192.168.64.10/24", "10.0.0"] {
            assert_eq!(format!("{:#}", value(&v4, &w(bad)).unwrap_err()), format!("'{bad}' is not an IPv4 address"));
        }
        assert_eq!(v4.describe(), "an IPv4 address");
    }

    /// A setting's name abbreviates as command words do: a unique beginning is the name (stored as
    /// the name); several are named; an entry's name is never guessed at.
    /// A section that takes any key (RFC 42's `settings`): a key of its own is a setting, as written,
    /// with the section's type; the section's own settings come first; `?` says so.
    #[test]
    fn a_free_section_takes_any_key() {
        let t = tree();
        let w = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        let r = t.resolve(&w("services openssh settings ClientAliveInterval 60")).unwrap();
        assert_eq!((r.loc.last().map(String::as_str), r.values.clone()), (Some("ClientAliveInterval"), w("60")));
        assert_eq!(value(r.ty, &r.values).unwrap(), serde_json::json!(60), "the section's type: a number here");
        assert_eq!(t.resolve(&w("services openssh settings LogLevel INFO")).unwrap().loc, w("services openssh settings LogLevel"), "its own first");
        assert!(t.next(&w("services openssh settings")).iter().any(|(x, _)| x == "<key>"));
        assert!(t.resolve(&w("services openssh settin ClientAliveInterval 60")).is_ok(), "the section itself still abbreviates");
        // Inside it, names are exact: a beginning of one of its own is a key of its own, as written
        // (`Port` must not become a declared `Ports`). Tab completes its own names.
        assert_eq!(t.resolve(&w("services openssh settings LogLev x")).unwrap().loc.last().map(String::as_str), Some("LogLev"));
    }

    #[test]
    fn names_abbreviate_entries_do_not() {
        let t = tree();
        let w = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(t.resolve(&w("virt dock en true")).unwrap().loc, w("virtualisation docker enable"));
        assert_eq!(t.locate(&w("serv htt")).unwrap().0, w("services httpd"), "a place, as the tree names it");
        let e = format!("{:#}", t.resolve(&w("sy x")).unwrap_err());
        assert_eq!(e, "'sy' is ambiguous: system, systemd");
        assert_eq!(t.resolve(&w("users users al extraGroups wheel")).unwrap().loc, w("users users al extraGroups"), "an entry: as written");
        assert!(t.next(&w("virt")).iter().any(|(x, _)| x == "docker"), "? after an abbreviation");
        // A section NixOS does not describe says what it holds.
        let docker = t.next(&w("virtualisation")).into_iter().find(|(x, _)| x == "docker").unwrap().1;
        assert_eq!(docker, "Settings: enable, rootless");
    }

    #[test]
    fn what_may_come_next() {
        let t = tree();
        let words = |v: Vec<(String, String)>| v.into_iter().map(|(w, _)| w).collect::<Vec<_>>();
        assert_eq!(words(t.next(&w("virtualisation docker"))), ["enable", "rootless"]);
        assert_eq!(words(t.next(&w("services httpd virtualHosts"))), ["<name>"], "an entry is named");
        assert_eq!(words(t.next(&w("services httpd virtualHosts example.com"))), ["documentRoot", "forceSSL", "serverAliases"]);
        assert_eq!(words(t.next(&w("networking firewall allowedTCPPorts"))), ["<[ a number from 0 to 65535 … ]>"]);
        assert_eq!(words(t.next(&w("services openssh settings LogLevel"))), ["QUIET", "FATAL", "ERROR", "INFO", "VERBOSE", "DEBUG", "DEBUG1", "DEBUG2", "DEBUG3"]);
        let help = t.next(&w("virtualisation docker"));
        assert!(help[0].1.starts_with("This option enables docker"), "{help:?}");
        assert!(t.next(&w("virtualisation docker enable true")).is_empty());
    }

    #[test]
    fn the_whole_schema_reads() {
        // The real file this system generates, when there is one (a TorqOS machine): every setting
        // must load, and the common ones resolve.
        let path = std::env::var("TORQ_NIXOS_SCHEMA").unwrap_or_else(|_| "/etc/torq/nixos-schema.json".into());
        let Ok(json) = std::fs::read_to_string(&path) else { return };
        let started = std::time::Instant::now();
        let t = Tree::parse(&json).unwrap();
        eprintln!("parsed {path} in {:?}", started.elapsed());
        assert!(t.resolve(&w("virtualisation docker enable true")).is_ok());
        assert!(t.resolve(&w("services openssh settings PasswordAuthentication false")).is_ok());
    }
}

#[cfg(test)]
#[test]
fn the_fence_is_the_modules() {
    let t = Tree::parse(include_str!("nixos-fixture.json")).unwrap();
    let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
    for fenced in [
        "torq broker rebuildCommand", "torq broker", "environment etc torq/broker.json text", "environment etc torq",
        "systemd services torq-broker-init enable", "systemd units torq-broker-init.service text", "nixpkgs config allowUnfree",
        "_module args", "torq packages user", "system build nixos-rebuild", "nix package", "systemd package",
        "services httpd virtualHosts _x",
    ] {
        assert!(t.fenced(&w(fenced)), "{fenced}");
    }
    for open in ["torq dev enable", "environment etc motd-torq/x text", "systemd services myapp enable", "torq", "environment etc"] {
        assert!(!t.fenced(&w(open)), "{open}");
    }
}
