//! How `torq` looks (docs/CLI-DESIGN.md): plain language, one visual language, colour with
//! meaning only — and plain text when colour is off (NO_COLOR, not a terminal), so nothing depends
//! on colour to be understood.
//!
//! The layout, in one place (CLI-DESIGN.md, "Layout"): lines fit the window, 20 to 80 columns
//! (`fit`); a sentence is wrapped by whoever writes it, under where its text starts (`Ui::line`,
//! `hang`); every table is a `Table` (data columns never broken, a text column wrapped); data (a
//! configuration, a comparison, a diff) is never wrapped. Without a window to fit (a script, or a filter reading the output) nothing is wrapped:
//! a line stays one line. Everything here but `columns` is a pure function, for snapshot tests.

use std::time::Duration;

/// The widest a line is laid out: wider windows get 80 columns too, so text stays readable.
pub const WIDTH: usize = 80;
/// The narrowest: below it, lines are laid out for 20 columns and the terminal wraps them.
pub const NARROWEST: usize = 20;

#[derive(Clone, Copy)]
pub struct Ui {
    pub color: bool,
    /// The window's width to fit (`fit`), or None: nothing to fit, nothing wrapped.
    pub fit: Option<usize>,
}

impl Ui {
    /// No colour, nothing to fit: for tests, which must not depend on the terminal they run in.
    #[cfg(test)]
    pub fn plain() -> Self {
        Ui { color: false, fit: None }
    }
    /// No colour, fitting `fit`: text the shell cleans, then paints, before printing.
    pub fn plain_at(fit: Option<usize>) -> Self {
        Ui { color: false, fit }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color { format!("\x1b[{code}m{s}\x1b[0m") } else { s.to_string() }
    }
    pub fn green(&self, s: &str) -> String {
        self.paint("32", s)
    }
    pub fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
    pub fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    pub fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    pub fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    pub fn cyan_bold(&self, s: &str) -> String {
        self.paint("1;36", s)
    }
    pub fn blue(&self, s: &str) -> String {
        self.paint("34", s)
    }

    /// For what is printed straight to standard output (`err`: standard error): colour on a
    /// terminal only, and never with NO_COLOR set (any value) or TERM=dumb; that terminal's width.
    pub fn for_terminal(err: bool) -> Self {
        use std::io::IsTerminal;
        let tty = if err { std::io::stderr().is_terminal() } else { std::io::stdout().is_terminal() };
        let off = std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").is_ok_and(|t| t == "dumb");
        Ui { color: tty && !off, fit: fit(columns(err)) }
    }

    /// How wide a table's line is: the window's (`fit`), else 80.
    pub fn width(&self) -> usize {
        self.fit.unwrap_or(WIDTH)
    }

    /// A sentence after `lead` (an indent, a mark, `error: `), its own line(s): wrapped under where
    /// it starts when there is a window to fit (`hang`). No newline at the end.
    pub fn line(&self, lead: &str, text: &str) -> String {
        let mut s = hang(lead, text, self.fit, usize::MAX);
        s.pop();
        s
    }

    /// `✓ what`, a sentence.
    pub fn ok(&self, what: &str) -> String {
        self.line(&format!("  {} ", self.green("✓")), what)
    }
    /// `⚠ what`, a sentence.
    pub fn warn(&self, what: &str) -> String {
        self.line(&format!("  {} ", self.yellow("⚠")), what)
    }
    /// `error: what`; each further line of `what` (`  because: …`) under its own indent.
    pub fn error(&self, what: &str) -> String {
        let mut out = String::new();
        for (i, l) in what.lines().enumerate() {
            let t = l.trim_start();
            let lead = if i == 0 { format!("{} ", self.red("error:")) } else { l[..l.len() - t.len()].to_string() };
            out.push_str(&hang(&lead, t, self.fit, usize::MAX));
        }
        if out.is_empty() {
            out = format!("{}\n", self.red("error:"));
        }
        out
    }
}

/// The machine's own text console (TERM=linux) has a small font without ✓ ⚠ ✗ ↶ …: there, plain
/// characters stand in. Any other terminal (over SSH, a desktop's) shows them as they are.
pub fn on_console() -> bool {
    std::env::var("TERM").is_ok_and(|t| t == "linux")
}

pub fn for_console(text: &str) -> String {
    console_text(text, on_console())
}

/// `text` as `console` (the machine's own) shows it.
fn console_text(text: &str, console: bool) -> String {
    if !console {
        return text.to_string();
    }
    text.replace('✓', "+").replace('⚠', "!").replace('✗', "x").replace('↶', "<").replace('…', "...")
}

/// How many columns the terminal on standard output (`err`: standard error) has, if it is one —
/// read each time, as the window may have been resized.
pub fn columns(err: bool) -> Option<usize> {
    let size = if err { rustix::termios::tcgetwinsize(std::io::stderr()) } else { rustix::termios::tcgetwinsize(std::io::stdout()) };
    size.ok().map(|w| usize::from(w.ws_col)).filter(|&c| c > 0)
}

/// How many lines the terminal on standard output shows; None if it is not one.
pub fn rows() -> Option<usize> {
    rustix::termios::tcgetwinsize(std::io::stdout()).ok().map(|w| usize::from(w.ws_row)).filter(|&r| r > 0)
}

/// The width to fit for a window `columns` wide: 20 to 80. None (no window): None.
pub fn fit(columns: Option<usize>) -> Option<usize> {
    columns.map(|c| c.clamp(NARROWEST, WIDTH))
}

/// `lead` then `text`, as lines, each ending in a newline. With a window `fit` wide and `text` too
/// long for the line: wrapped under where it starts (never back to the left edge), or — with less
/// than 20 columns beside a `lead` wider than 4 — on the lines after it, indented 4; at most `max` lines, the last
/// ending in `…` when there was more (one line: always beside its lead). Otherwise one line, exactly
/// as it is.
///
/// What follows a `lead` that ends in `! ` starts where its text started: paint.rs keeps those
/// lines yellow by that.
pub fn hang(lead: &str, text: &str, fit: Option<usize>, max: usize) -> String {
    let Some(fit) = fit.filter(|&f| width(lead) + width(text) > f) else { return format!("{lead}{text}\n") };
    // Under it only gains room when `lead` is wider than the 4 columns it would be indented.
    let beside = max <= 1 || width(lead) <= 4 || fit.saturating_sub(width(lead)) >= NARROWEST;
    let column = if beside { width(lead) } else { 4 };
    let room = fit.saturating_sub(column).max(1);
    let mut lines = wrap(text, room, room);
    if lines.len() > max.max(1) {
        lines.truncate(max.max(1));
        if let Some(last) = lines.last_mut() {
            while width(last) + 1 > room {
                last.pop();
            }
            let keep = last.trim_end().len();
            last.truncate(keep);
            last.push('…');
        }
    }
    let mut out = String::new();
    // (A lead of only spaces has nothing to show on a line of its own.)
    if !beside && !lead.trim().is_empty() {
        out.push_str(&format!("{}\n", lead.trim_end()));
    }
    for (i, line) in lines.iter().enumerate() {
        let start = if i == 0 && beside { lead.to_string() } else { " ".repeat(column) };
        out.push_str(&format!("{start}{line}\n"));
    }
    out
}

/// A column of a `Table`: its heading (empty: none) and how its cells line up. `Text`, the last
/// column only, is a sentence: wrapped under where it starts when there is a window to fit.
#[derive(Clone, Copy)]
pub enum Col<'a> {
    Left(&'a str),
    Right(&'a str),
    Text(&'a str),
}

/// A table, the one layout of every table torqcli shows (ADR-025): data columns as wide as their
/// widest cell, never broken (a filter reads whole lines; an address is not cut), two spaces apart;
/// then, if there is one, the text column, wrapped (`hang`), never cut unless `lines` says. A column
/// may be capped (`widest`): a cell wider than that stands on its own line, the rest of its row on
/// the next, in the columns. A column empty in every row takes no room. A heading row when any
/// column has a heading; lines of their own (a day's name) between rows.
pub struct Table<'a> {
    cols: Vec<Col<'a>>,
    rows: Vec<Row>,
    indent: usize,
    widest: Vec<Option<usize>>,
    lines: usize,
}

enum Row {
    Cells(Vec<String>),
    Line(String),
}

impl<'a> Table<'a> {
    pub fn new(cols: &[Col<'a>]) -> Self {
        Table { cols: cols.to_vec(), rows: vec![], indent: 0, widest: vec![None; cols.len()], lines: usize::MAX }
    }

    /// Columns from the left edge.
    pub fn indent(mut self, n: usize) -> Self {
        self.indent = n;
        self
    }

    /// Column `col` is at most `n` wide: a wider cell stands on its own line.
    pub fn widest(mut self, col: usize, n: usize) -> Self {
        self.widest[col] = Some(n);
        self
    }

    /// At most `n` lines of text per row, the last ending in `…` when there was more (a summary).
    pub fn lines(mut self, n: usize) -> Self {
        self.lines = n;
        self
    }

    /// A row: a cell per column (missing ones empty).
    pub fn row(&mut self, cells: Vec<String>) {
        self.rows.push(Row::Cells(cells));
    }

    /// A line of its own between rows, as it is (a day's name).
    pub fn line(&mut self, text: String) {
        self.rows.push(Row::Line(text));
    }

    pub fn render(&self, ui: &Ui) -> String {
        let data = self.cols.len() - usize::from(matches!(self.cols.last(), Some(Col::Text(_))));
        let heads: Vec<String> = self
            .cols
            .iter()
            .map(|c| match *c {
                Col::Left(h) | Col::Right(h) | Col::Text(h) => h.to_string(),
            })
            .collect();
        let heading = heads.iter().any(|h| !h.is_empty()).then_some(Row::Cells(heads));
        let rows: Vec<&Row> = heading.iter().chain(&self.rows).collect();
        let widths: Vec<usize> = (0..data)
            .map(|i| {
                let w = rows.iter().filter_map(|r| match r {
                    Row::Cells(c) => Some(c.get(i).map_or(0, |c| width(c))),
                    Row::Line(_) => None,
                });
                let w = w.max().unwrap_or(0);
                self.widest[i].map_or(w, |cap| w.min(cap))
            })
            .collect();
        // Where each column starts (an empty one takes no room).
        let starts: Vec<usize> = (0..=data).map(|i| self.indent + widths[..i].iter().filter(|w| **w > 0).map(|w| w + 2).sum::<usize>()).collect();
        let mut out = String::new();
        for r in rows {
            let cells = match r {
                Row::Line(l) => {
                    out.push_str(&format!("{l}\n"));
                    continue;
                }
                Row::Cells(c) => c,
            };
            let mut line = " ".repeat(self.indent);
            for i in (0..data).filter(|i| widths[*i] > 0 || self.widest[*i].is_some()) {
                let cell = cells.get(i).map_or("", String::as_str);
                let gap = widths[i].saturating_sub(width(cell));
                if width(cell) > widths[i] {
                    // Wider than its column may be: on its own line; the row goes on below it.
                    line.push_str(cell);
                    out.push_str(&format!("{}\n", line.trim_end()));
                    line = " ".repeat(starts[i + 1]);
                    continue;
                }
                match self.cols[i] {
                    Col::Right(_) => line.push_str(&format!("{}{cell}", " ".repeat(gap))),
                    _ => line.push_str(&format!("{cell}{}", " ".repeat(gap))),
                }
                line.push_str("  ");
            }
            let text = cells.get(data).map_or("", String::as_str);
            if data < self.cols.len() && !text.is_empty() {
                // Beside the cells; where a cell went on its own line and the window leaves too little
                // room beside the columns, under them (hang).
                out.push_str(&hang(&line, text, ui.fit, self.lines));
            } else if !line.trim().is_empty() {
                out.push_str(&format!("{}\n", line.trim_end()));
            }
        }
        out
    }
}

/// `text` in lines: the first at most `first` characters, the rest at most `room`, broken between
/// words (spaces collapsed; a word longer than a line is broken inside).
pub fn wrap(text: &str, first: usize, room: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![String::new()];
    let mut words: Vec<String> = text.split_whitespace().map(String::from).collect();
    words.reverse();
    while let Some(word) = words.pop() {
        let limit = if lines.len() == 1 { first } else { room }.max(1);
        let Some(line) = lines.last_mut() else { break };
        let len = width(line);
        let need = if len == 0 { width(&word) } else { len + 1 + width(&word) };
        if need <= limit {
            if len > 0 {
                line.push(' ');
            }
            line.push_str(&word);
        } else if len == 0 {
            // Longer than a whole line: as much as fits, the rest on the next.
            line.push_str(&word.chars().take(limit).collect::<String>());
            words.push(word.chars().skip(limit).collect());
            lines.push(String::new());
        } else {
            words.push(word);
            lines.push(String::new());
        }
    }
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Display width, counting each character as one column (the text here is our own wording and
/// already-cleaned names; wide characters only make a line a little longer, never break it), and
/// our own colour codes (`\x1b[…m`) as none.
pub fn width(s: &str) -> usize {
    let mut n = 0;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // A control sequence: `[`, then parameters, up to its final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            n += 1;
        }
    }
    n
}

/// Elapsed time as people say it: "3s", "1m 12s".
pub fn elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 { format!("{s}s") } else { format!("{}m {:02}s", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One layout for every table: data columns as wide as their widest cell (never broken), right-
    /// aligned where numbers are, a heading, the text column wrapped under where it starts, a capped
    /// column's wide cell on its own line, lines of their own between rows.
    #[test]
    fn a_table() {
        let mut t = Table::new(&[Col::Left("Name"), Col::Right("Size"), Col::Text("What")]).indent(2);
        t.row(vec!["a".into(), "5G".into(), "the first one, with a longer sentence than fits".into()]);
        t.line("  Later".into());
        t.row(vec!["bbbbbb".into(), "120M".into(), String::new()]);
        assert_eq!(
            t.render(&at(40)),
            "  Name    Size  What\n  a         5G  the first one, with a\n                longer sentence than\n                fits\n  Later\n  bbbbbb  120M\n"
        );
        assert_eq!(t.render(&Ui::plain()).lines().nth(1), Some("  a         5G  the first one, with a longer sentence than fits"), "no window: one line");
        let mut capped = Table::new(&[Col::Left(""), Col::Text("")]).indent(2).widest(0, 4);
        capped.row(vec!["ab".into(), "Short.".into()]);
        capped.row(vec!["abcdefgh".into(), "Beside the column, on the next line.".into()]);
        assert_eq!(capped.render(&at(80)), "  ab    Short.\n  abcdefgh\n        Beside the column, on the next line.\n");
        let mut data = Table::new(&[Col::Left("Interface"), Col::Left("Address")]);
        data.row(vec!["eth0".into(), "fe80::1234:5678:9abc:def0/64".into()]);
        assert_eq!(data.render(&at(20)).lines().nth(1), Some("eth0       fe80::1234:5678:9abc:def0/64"), "data is never wrapped");
    }

    fn at(fit: usize) -> Ui {
        Ui { color: false, fit: Some(fit) }
    }

    #[test]
    fn plain_has_no_escapes_colour_has() {
        let p = Ui::plain();
        assert!(!p.warn("x").contains('\x1b') && !p.error("x").contains('\x1b'));
        assert!(Ui { color: true, ..Ui::plain() }.ok("x").contains("\x1b[32m"));
    }

    #[test]
    fn one_width_rule() {
        assert_eq!(fit(None), None, "no window: nothing to fit");
        assert_eq!(fit(Some(200)), Some(80));
        assert_eq!(fit(Some(50)), Some(50));
        assert_eq!(fit(Some(8)), Some(20));
        assert_eq!(Ui::plain().width(), 80, "a table without a window: 80");
        assert_eq!(at(50).width(), 50);
    }

    #[test]
    fn colour_takes_no_room() {
        assert_eq!(width("\x1b[31merror:\x1b[0m x"), 8);
        assert_eq!(width("✓ ok"), 4);
        let red = Ui { color: true, fit: Some(30) };
        let e = red.error("the configuration has changed since this change was prepared");
        assert!(e.lines().all(|l| width(l) <= 30), "{e:?}");
        assert!(e.lines().nth(1).is_some_and(|l| l.starts_with("       c")), "under the text, past `error: `: {e:?}");
    }

    #[test]
    fn wrap_and_hang() {
        assert_eq!(wrap("a bb  ccc dddd", 6, 6), ["a bb", "ccc", "dddd"], "spaces collapsed, between words");
        assert_eq!(wrap("abcdefgh", 3, 3), ["abc", "def", "gh"], "a word longer than a line, broken");
        assert_eq!(wrap("ab cd", 0, 0), ["a", "b", "c", "d"], "no room still ends");
        assert_eq!(wrap("", 5, 5), [""]);
        let lead = "0   d  ";
        let text = "one two three four five six seven eight nine ten eleven";
        assert_eq!(hang(lead, text, None, usize::MAX), format!("{lead}{text}\n"), "no window: one line");
        assert_eq!(hang(lead, "a  b", Some(30), usize::MAX), "0   d  a  b\n", "fits: exactly as it is");
        assert_eq!(hang(lead, text, Some(30), usize::MAX), "0   d  one two three four five\n       six seven eight nine\n       ten eleven\n");
        assert_eq!(hang(lead, text, Some(26), usize::MAX), "0   d\n    one two three four\n    five six seven eight\n    nine ten eleven\n", "under it");
        assert_eq!(hang(lead, text, Some(30), 2), "0   d  one two three four five\n       six seven eight nine…\n", "at most 2 lines");
        assert_eq!(hang("! ", "TorqOS comes from a folder (path:): whoever can write to it decides.", Some(30), usize::MAX), "! TorqOS comes from a folder\n  (path:): whoever can write\n  to it decides.\n");
    }

    #[test]
    fn sentences_fit_the_window_and_only_with_one() {
        let long = "This machine is exactly as its history says, and has been for a while.";
        for w in [20, 33, 50, 80] {
            for s in [at(w).ok(long), at(w).warn(long), at(w).line("    · ", long), at(w).error(&format!("{long}\n  because: {long}"))] {
                assert!(s.lines().all(|l| width(l) <= w), "{w}: {s}");
            }
        }
        assert_eq!(Ui::plain().ok(long), format!("  ✓ {long}"), "no window: one line");
        assert_eq!(Ui::plain().error("a\n  because: b"), "error: a\n  because: b\n");
        assert_eq!(at(33).error("short\n  because: the reason, long enough to wrap"), "error: short\n  because: the reason, long\n            enough to wrap\n".replace("            ", "  "));
        assert_eq!(Ui::plain().error(""), "error:\n");
    }

    #[test]
    fn the_console_gets_plain_characters() {
        let text = "  ✓ fine\n  ⚠ not\n    21:07   ↶ undone ✗ failed …";
        assert_eq!(console_text(text, true), "  + fine\n  ! not\n    21:07   < undone x failed ...");
        assert_eq!(console_text(text, false), text, "any other terminal: as it is");
    }

    #[test]
    fn elapsed_reads_like_people_say_it() {
        assert_eq!(elapsed(Duration::from_secs(3)), "3s");
        assert_eq!(elapsed(Duration::from_secs(72)), "1m 12s");
    }
}
