//! A saved configuration (`save <file>`, `load override|merge <file>`; docs/CONFIG-IS-THE-DEVICE.md,
//! step 6): the whole tree in the brace format `show` prints, then the content of every config file
//! attached in it, so the file alone recreates the device:
//!
//! ```text
//! networking {
//!     hostName torq-utm;
//! }
//! config hardware-configuration.nix;
//! ## config file hardware-configuration.nix
//! ##| { config, lib, ... }:
//! ##| { … }
//! ```
//!
//! Other `##` lines are comments (the footer `show` prints). Pure: text in, entries out.

use crate::tree::Val;
use anyhow::{bail, Result};

/// The marker of a config file's content, and of each of its lines.
const FILE: &str = "## config file ";
const LINE: &str = "##|";
/// After a config file's last line, if the file does not end with a line break.
const NO_EOL: &str = "##! no newline at end of file";
/// A Nix file of the base that the saved candidate deletes.
const DELETED: &str = "## config file deleted ";
/// The largest saved configuration, as `save` writes and `load` reads it.
pub const MAX: usize = 16 << 20;

/// A saved configuration as text: the tree (`show`'s brace format) and the config files' content.
pub fn render(tree: &str, files: &[(String, String)], deleted: &[String]) -> String {
    let mut out = tree.to_string();
    for path in deleted {
        out.push_str(&format!("{DELETED}{path}\n"));
    }
    for (path, content) in files {
        out.push_str(&format!("{FILE}{path}\n"));
        // Byte for byte: each line as it is (a `\r` kept), and a last line without a break said so.
        for line in content.split_inclusive('\n') {
            out.push_str(&format!("{LINE} {}\n", line.strip_suffix('\n').unwrap_or(line)));
        }
        if !content.is_empty() && !content.ends_with('\n') {
            out.push_str(&format!("{NO_EOL}\n"));
        }
    }
    out
}

/// What a saved configuration holds: its entries (path, value) and its config files' content.
#[derive(Debug, Default, PartialEq)]
pub struct Saved {
    pub entries: Vec<(Vec<String>, Val)>,
    pub files: Vec<(String, String)>,
    /// Nix files of the configuration the saved candidate deleted.
    pub deleted: Vec<String>,
}

/// Read a saved configuration. An error names the line.
pub fn parse(text: &str) -> Result<Saved> {
    let mut saved = Saved::default();
    let mut tree = String::new();
    // The tree's lines, each with its line number (comments and file blocks left out).
    let mut numbers: Vec<usize> = Vec::new();
    let mut file: Option<(String, String)> = None;
    // Inside a `/* … */` annotation that spans lines.
    let mut annotation = false;
    let pieces: Vec<&str> = text.strip_suffix('\n').unwrap_or(text).split('\n').collect();
    for (i, raw) in pieces.into_iter().enumerate() {
        // (A config file's lines keep a `\r`; the tree's and the markers' do not need one.)
        if let Some(rest) = raw.strip_prefix(LINE) {
            let Some((_, content)) = file.as_mut() else { bail!("line {}: a config file's line before its `{FILE}<path>`", i + 1) };
            content.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            content.push('\n');
            continue;
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line == NO_EOL {
            match file.as_mut() {
                Some((_, content)) if content.ends_with('\n') => {
                    content.pop();
                }
                _ => bail!("line {}: `{NO_EOL}` after no config file's line", i + 1),
            }
            continue;
        }
        if let Some(path) = line.strip_prefix(DELETED) {
            saved.files.extend(file.take());
            crate::attach::valid(path).map_err(|e| anyhow::anyhow!("line {}: {e:#}", i + 1))?;
            saved.deleted.push(path.to_string());
            continue;
        }
        if let Some(path) = line.strip_prefix(FILE) {
            saved.files.extend(file.take());
            crate::attach::valid(path).map_err(|e| anyhow::anyhow!("line {}: {e:#}", i + 1))?;
            file = Some((path.to_string(), String::new()));
            continue;
        }
        saved.files.extend(file.take());
        // Comments, on lines of their own (where no statement is): `#` and `##` lines, and `/* … */`
        // annotations as network devices write them, over one line or several.
        let t = line.trim();
        if annotation {
            annotation = !t.ends_with("*/");
            continue;
        }
        if t.starts_with("/*") {
            annotation = !t.ends_with("*/") || t == "/*";
            continue;
        }
        if t.starts_with('#') {
            continue;
        }
        tree.push_str(line);
        tree.push('\n');
        numbers.push(i + 1);
    }
    saved.files.extend(file.take());
    if annotation {
        bail!("a `/*` annotation is not closed (`*/`)");
    }
    let mut seen = std::collections::BTreeSet::new();
    for (p, _) in &saved.files {
        if !seen.insert(p) {
            bail!("{p}: its content is in the file twice");
        }
    }
    saved.entries = statements(&tree).map_err(|(line, e)| anyhow::anyhow!("line {}: {e}", numbers.get(line).copied().unwrap_or(0)))?;
    Ok(saved)
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Open,
    Close,
    End,
    ListOpen,
    ListClose,
}

/// The brace format's tokens, each with its line (0-based, of `text`).
fn tokens(text: &str) -> std::result::Result<Vec<(usize, Tok)>, (usize, String)> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let mut chars = line.chars().peekable();
        while let Some(&c) = chars.peek() {
            match c {
                c if c.is_whitespace() => {
                    chars.next();
                }
                '{' | '}' | ';' | '[' | ']' => {
                    chars.next();
                    out.push((n, match c { '{' => Tok::Open, '}' => Tok::Close, ';' => Tok::End, '[' => Tok::ListOpen, _ => Tok::ListClose }));
                }
                '"' => {
                    chars.next();
                    let mut w = String::new();
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some('\\') => match chars.next() {
                                Some(e @ ('\\' | '"')) => w.push(e),
                                _ => return Err((n, "a `\\` in quotes is only for `\\\\` or `\\\"`".into())),
                            },
                            Some(c) => w.push(c),
                            None => return Err((n, "a quote is not closed on its line".into())),
                        }
                    }
                    out.push((n, Tok::Word(w)));
                }
                _ => {
                    let mut w = String::new();
                    while let Some(&c) = chars.peek() {
                        if c.is_whitespace() || "{};[]\"".contains(c) {
                            break;
                        }
                        w.push(c);
                        chars.next();
                    }
                    out.push((n, Tok::Word(w)));
                }
            }
        }
    }
    Ok(out)
}

/// The statements of the brace format: each a whole path and its value.
fn statements(text: &str) -> std::result::Result<Vec<(Vec<String>, Val)>, (usize, String)> {
    let toks = tokens(text)?;
    let mut out = Vec::new();
    // The places open (`a b {` opens `a b`): how many words each added.
    let mut path: Vec<String> = Vec::new();
    let mut opened: Vec<usize> = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut it = toks.into_iter().peekable();
    let mut last = 0;
    while let Some((n, t)) = it.next() {
        last = n;
        match t {
            Tok::Word(w) => words.push(w),
            Tok::Open => {
                if words.is_empty() {
                    return Err((n, "`{` without a name before it".into()));
                }
                opened.push(words.len());
                path.append(&mut words);
            }
            Tok::Close => {
                if !words.is_empty() {
                    return Err((n, format!("`{}` has no `;` before the `}}`", words.join(" "))));
                }
                let Some(k) = opened.pop() else { return Err((n, "a `}` that closes nothing".into())) };
                path.truncate(path.len() - k);
            }
            Tok::End => {
                if words.len() < 2 {
                    return Err((n, "a statement is a name and its value (`hostName torq;`)".into()));
                }
                let v = words.pop().unwrap_or_default();
                out.push((path.iter().cloned().chain(words.drain(..)).collect(), Val::One(v)));
            }
            Tok::ListOpen => {
                if words.is_empty() {
                    return Err((n, "`[` without a name before it".into()));
                }
                let mut vs = Vec::new();
                loop {
                    match it.next() {
                        Some((_, Tok::Word(w))) => vs.push(w),
                        Some((_, Tok::ListClose)) => break,
                        _ => return Err((n, "a `[` list holds values and ends with `]`".into())),
                    }
                }
                if !matches!(it.next(), Some((_, Tok::End))) {
                    return Err((n, "a `[ … ]` list ends with `;`".into()));
                }
                out.push((path.iter().cloned().chain(words.drain(..)).collect(), Val::Many(vs)));
            }
            Tok::ListClose => return Err((n, "a `]` that closes nothing".into())),
        }
    }
    if !words.is_empty() {
        return Err((last, format!("`{}` has no `;` at its end", words.join(" "))));
    }
    if !opened.is_empty() {
        return Err((last, format!("`{}` is not closed: a `}}` is missing", path.join(" "))));
    }
    Ok(out)
}

/// The words `set` takes for an entry: its path and value (a list as `[ … ]`).
pub fn set_words(path: &[String], v: &Val) -> Vec<String> {
    let mut w = path.to_vec();
    match v {
        Val::One(v) => w.push(v.clone()),
        Val::Many(vs) => {
            w.push("[".into());
            w.extend(vs.iter().cloned());
            w.push("]".into());
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn reads_what_show_writes() {
        let entries = vec![
            (w("boot loader systemd-boot enable"), Val::One("true".into())),
            (w("config"), Val::One("hardware-configuration.nix".into())),
            (vec!["fileSystems".into(), "/boot".into(), "options".into()], Val::Many(vec!["fmask=0077".into(), "dmask=0077".into()])),
            (w("networking hostName"), Val::One("torq-utm".into())),
            (w("services httpd virtualHosts"), Val::Many(vec![])),
            (vec!["environment".into(), "variables".into(), "GREETING".into()], Val::One("hello \"you\" \\ there; {x}".into())),
            (vec!["services".into(), "a b".into(), "x".into()], Val::One("".into())),
            (vec!["environment".into(), "variables".into(), "A B".into()], Val::One("v".into())),
        ];
        let mut sorted = entries.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let tree = crate::tree::render_entries(&sorted);
        let text = render(&format!("{tree}## software: torq 2026-10-31\n"), &[("hardware-configuration.nix".into(), "{ ... }:\n{\n  # a comment ##| not a marker\n}\n".into())], &["old.nix".into()]);
        let saved = parse(&text).unwrap_or_else(|e| panic!("{e:#}\n{text}"));
        assert_eq!(saved.entries, sorted, "{text}");
        assert_eq!(saved.files, [("hardware-configuration.nix".to_string(), "{ ... }:\n{\n  # a comment ##| not a marker\n}\n".to_string())]);
        assert_eq!(saved.deleted, ["old.nix"], "a deletion is saved too");
        // Byte for byte: CRLF kept, a missing last line break kept, an empty file.
        for content in ["a\r\nb\r\n", "no break at the end", "two\nlines", "", "\n"] {
            let text = render("", &[("a.nix".into(), content.into())], &[]);
            assert_eq!(parse(&text).unwrap().files, [("a.nix".to_string(), content.to_string())], "{content:?}: {text:?}");
        }
    }

    /// What people paste: comments on lines of their own, `#` at any indent and `/* … */` annotations
    /// (over one line or several), are left out; an annotation never closed is refused.
    #[test]
    fn comments_on_their_own_lines() {
        let text = "## Last changed by alice\n# a note\nnetworking {\n    /* the name */\n    # indented\n    hostName torq;\n    /* a longer\n       note */\n}\n";
        assert_eq!(parse(text).unwrap().entries, [(w("networking hostName"), Val::One("torq".into()))]);
        let e = format!("{:#}", parse("networking {\n    /* never closed\n    hostName torq;\n}\n").unwrap_err());
        assert!(e.contains("not closed"), "{e}");
        assert_eq!(parse("time {\n    timeZone \"a # b\";\n}\n").unwrap().entries, [(w("time timeZone"), Val::One("a # b".into()))], "a # inside a value is the value's");
    }

    #[test]
    fn says_where_it_is_wrong() {
        for (text, line, what) in [
            ("a {\n  b c;\n", 2, "not closed"),
            ("a {\n  b c\n}\n", 3, "no `;`"),
            ("}\n", 1, "closes nothing"),
            ("a [ b c\n", 1, "ends with `]`"),
            ("a [ b ]\n", 1, "ends with `;`"),
            ("x;\n", 1, "a name and its value"),
            ("a \"b;\n", 1, "not closed"),
            ("##| x\n", 1, "before its"),
            ("## config file ../x.nix\n", 1, "inside the config repo"),
        ] {
            let e = format!("{:#}", parse(text).unwrap_err());
            assert!(e.contains(&format!("line {line}")) && e.contains(what), "{text:?}: {e}");
        }
        assert!(parse("## config file a.nix\n##| x\n## config file a.nix\n").is_err(), "a file twice");
        assert_eq!(parse("## a comment\n\n").unwrap(), Saved::default());
    }

    #[test]
    fn set_takes_lists_in_brackets() {
        assert_eq!(set_words(&w("a b"), &Val::Many(vec!["x".into(), "y".into()])), w("a b [ x y ]"));
        assert_eq!(set_words(&w("a b"), &Val::One("x".into())), w("a b x"));
    }
}
