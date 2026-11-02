//! Colour for what torqcli prints, on a terminal (docs/CLI-DESIGN.md, "the terminal itself"): the
//! same text, its parts told apart — `[edit …]` sections, `+`/`-` changes, a configuration's places,
//! settings and values, comments, ✓ and ⚠, `?` lists. Applied last, to text already cleaned of
//! anything that could fool a terminal (history::clean), so a value can never bring its own colour
//! codes; and never when colour is off (NO_COLOR, not a terminal: `Ui::plain`). Pure, for tests.

use crate::ui::Ui;

/// `text` (lines already cleaned), coloured line by line.
pub fn paint(text: &str, ui: &Ui) -> String {
    if !ui.color {
        return text.to_string();
    }
    let mut out = String::new();
    let mut completions = false;
    // Where a `! warning`'s text starts: its next lines start there too (ui::hang), and stay yellow.
    let mut warning: Option<usize> = None;
    for line in text.split_inclusive('\n') {
        let (body, end) = line.strip_suffix('\n').map_or((line, ""), |b| (b, "\n"));
        if body.is_empty() {
            completions = false;
        }
        let t = body.trim_start();
        let indent = body.len() - t.len();
        let painted = if body == "Possible completions:" {
            completions = true;
            ui.bold(body)
        } else if completions {
            completion(body, ui)
        } else if warning == Some(indent) && !t.is_empty() {
            format!("{}{}", &body[..indent], ui.yellow(t))
        } else {
            warning = t.starts_with("! ").then_some(indent + 2);
            one(body, ui)
        };
        out.push_str(&painted);
        out.push_str(end);
    }
    out
}

fn one(line: &str, ui: &Ui) -> String {
    let t = line.trim_start();
    let indent = &line[..line.len() - t.len()];
    // What changes: sections, and the lines they hold (`show | compare`, `| display diff`).
    if t.starts_with("[edit") && t.ends_with(']') {
        return format!("{indent}{}", ui.cyan_bold(t));
    }
    if line.starts_with("+++ ") || line.starts_with("--- ") || line.starts_with("diff ") {
        return ui.bold(line);
    }
    if line.starts_with("@@") {
        return ui.cyan(line);
    }
    if line.starts_with('+') {
        return ui.green(line);
    }
    if line.starts_with('-') {
        return ui.red(line);
    }
    // Comments (`## software: …`), warnings in a question, the outcome of a command.
    if t.starts_with("##") {
        return ui.dim(line);
    }
    if let Some(rest) = t.strip_prefix("! ") {
        return format!("{indent}{} {}", ui.yellow("!"), ui.yellow(rest));
    }
    if ["commit complete", "load complete", "reverted:", "Wrote "].iter().any(|p| t.starts_with(p)) {
        return format!("{indent}{}", ui.green(t));
    }
    // A history's marks, and a status.
    if let Some((mark, rest)) = mark(t) {
        let m = match mark {
            '✓' => ui.green("✓"),
            '⚠' => ui.yellow("⚠"),
            '✗' => ui.red("✗"),
            _ => ui.yellow(&mark.to_string()),
        };
        return format!("{indent}{m}{rest}");
    }
    if let Some((before, rest)) = marked_inside(line) {
        return format!("{before}{}", one_mark(rest, ui));
    }
    // `| display set`.
    for kw in ["set ", "delete "] {
        if let Some(rest) = t.strip_prefix(kw) {
            return format!("{indent}{}{rest}", ui.cyan(kw));
        }
    }
    configuration(indent, t, ui).unwrap_or_else(|| line.to_string())
}

/// A line that starts with one of the marks (✓ ⚠ ✗ ↶), and what follows it.
fn mark(t: &str) -> Option<(char, &str)> {
    let c = t.chars().next()?;
    ['✓', '⚠', '✗', '↶'].contains(&c).then(|| (c, &t[c.len_utf8()..]))
}

/// A history row (`    21:07   ✓ reason …`): the mark is inside the line.
fn marked_inside(line: &str) -> Option<(&str, &str)> {
    let i = line.find(['✓', '✗', '↶'])?;
    line[..i].chars().all(|c| c.is_ascii_digit() || c == ':' || c == ' ').then(|| (&line[..i], &line[i..]))
}

fn one_mark(t: &str, ui: &Ui) -> String {
    let (m, rest) = mark(t).unwrap_or((' ', t));
    let m = match m {
        '✓' => ui.green("✓"),
        '✗' => ui.red("✗"),
        '↶' => ui.yellow("↶"),
        _ => String::new(),
    };
    format!("{m}{rest}")
}

/// The brace format: `place {` (bold), `}` (dim), `setting value;` (the setting cyan, `[ ]` and
/// `;` dim). None if the line is not one of them.
fn configuration(indent: &str, t: &str, ui: &Ui) -> Option<String> {
    if t == "}" {
        return Some(format!("{indent}{}", ui.dim("}")));
    }
    if let Some(place) = t.strip_suffix(" {") {
        return Some(format!("{indent}{} {}", ui.bold(place), ui.dim("{")));
    }
    let stmt = t.strip_suffix(';')?;
    let name_end = word_end(stmt)?;
    let (name, value) = (&stmt[..name_end], stmt[name_end..].strip_prefix(' ')?);
    let value = match value.strip_prefix("[ ").and_then(|v| v.strip_suffix(" ]")) {
        Some(list) => format!("{} {list} {}", ui.dim("["), ui.dim("]")),
        None if value == "[  ]" || value == "[ ]" => ui.dim(value),
        None => value.to_string(),
    };
    Some(format!("{indent}{} {value}{}", ui.cyan(name), ui.dim(";")))
}

/// Where the first word of `s` ends: a quoted word (`"a b"`) or a bare one.
fn word_end(s: &str) -> Option<usize> {
    if let Some(rest) = s.strip_prefix('"') {
        let mut escaped = false;
        for (i, c) in rest.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => return Some(i + 2),
                _ => escaped = false,
            }
        }
        None
    } else {
        s.find(' ')
    }
}

/// The prompt (`alice@host> `, `alice@host# `): the user cyan, the host blue, the mark bold.
pub fn prompt(p: &str, ui: &Ui) -> String {
    if !ui.color {
        return p.to_string();
    }
    let Some((user, rest)) = p.split_once('@') else { return p.to_string() };
    let host_end = rest.find(['>', '#']).unwrap_or(rest.len());
    let (host, mark) = rest.split_at(host_end);
    format!("{}{}{}{}", ui.cyan(user), ui.dim("@"), ui.blue(host), ui.bold(mark))
}

/// A `?` list line: `  word   help` — the word cyan, the help dim; a help's further lines (indented
/// under the help column, complete::render) dim.
fn completion(line: &str, ui: &Ui) -> String {
    let t = line.trim_start();
    let indent = &line[..line.len() - t.len()];
    if indent.len() > 2 {
        return format!("{indent}{}", ui.dim(t));
    }
    match t.find("  ") {
        Some(i) => {
            let (word, rest) = t.split_at(i);
            let help = rest.trim_start();
            format!("{indent}{}{}{}", ui.cyan(word), &rest[..rest.len() - help.len()], ui.dim(help))
        }
        None => format!("{indent}{}", ui.cyan(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    const SAMPLE: &str = "networking {\n    hostName torq-utm;\n    firewall {\n        allowedTCPPorts [ 80 443 ];\n    }\n}\n\"a b\" x;\n## software: torq 2026-10-31\n[edit services openssh]\n+   enable true;\n-   enable false;\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n! TorqOS comes from a folder\n  that; wraps\nset networking hostName x\ncommit complete (change #42)\n  ✓ This machine is exactly as its history says.\n  ⚠ not as its history says\n    21:07   ✓ H4 live check\n\nPossible completions:\n  merge  Set the file's settings\n  set    Run the file's lines that\n         wrap\n";

    #[test]
    fn the_same_text_coloured_and_nothing_when_colour_is_off() {
        let on = Ui { color: true, ..Ui::plain() };
        let painted = paint(SAMPLE, &on);
        assert_eq!(strip(&painted), SAMPLE, "colour only: not a character of the text changes");
        assert_eq!(paint(SAMPLE, &Ui::plain()), SAMPLE);
        let line = |needle: &str| painted.lines().find(|l| strip(l).contains(needle)).unwrap().to_string();
        assert!(line("[edit services").starts_with("\x1b[1;36m"), "a section: bold cyan");
        assert!(line("+   enable").starts_with("\x1b[32m") && line("-   enable").starts_with("\x1b[31m"));
        assert!(line("hostName").contains("\x1b[36mhostName\x1b[0m torq-utm\x1b[2m;"), "{:?}", line("hostName"));
        assert!(line("networking {").contains("\x1b[1mnetworking\x1b[0m"));
        assert!(line("allowedTCPPorts").contains("\x1b[2m[\x1b[0m 80 443 \x1b[2m]"));
        assert!(line("\"a b\"").contains("\x1b[36m\"a b\"\x1b[0m"), "a quoted name is one word");
        assert!(line("## software").starts_with("\x1b[2m"));
        assert!(line("✓ This machine").contains("\x1b[32m✓") && line("⚠").contains("\x1b[33m⚠"));
        assert!(line("21:07").contains("\x1b[32m✓"), "a history row's mark");
        assert!(line("  merge  Set").contains("\x1b[36mmerge\x1b[0m") && line("  merge  Set").contains("\x1b[2mSet the file"));
        assert!(line("  that; wraps").starts_with("  \x1b[33mthat; wraps"), "a warning's next line: yellow, not a setting");
        assert!(line("         wrap").starts_with("         \x1b[2mwrap"), "a help's next line: dim, not a word");
    }

    #[test]
    fn the_prompt_coloured_and_its_text_unchanged() {
        let on = Ui { color: true, ..Ui::plain() };
        for p in ["viper@torq-utm> ", "viper@torq-utm# "] {
            assert_eq!(strip(&prompt(p, &on)), p);
            assert!(prompt(p, &on).starts_with("\x1b[36mviper"), "{p}");
        }
        assert_eq!(prompt("viper@torq-utm> ", &Ui::plain()), "viper@torq-utm> ");
    }

    /// Cleaned text has no escape of its own to start with (history::clean): painting adds only ours.
    #[test]
    fn a_value_cannot_bring_colour() {
        let cleaned = crate::history::clean("hostName \x1b[31mred;");
        assert!(!cleaned.contains('\x1b'));
        assert!(!strip(&paint(&cleaned, &Ui { color: true, ..Ui::plain() })).contains('\x1b'));
    }
}
