//! `show system ledger` and `request system revert`: the ledger, as the broker prints it (`log --json`; the ledger
//! itself is root-only). Pure parts here; main.rs talks to the broker.

use anyhow::{bail, Context, Result};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Entry {
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    pub tier: String,
    pub actor: String,
    /// Who approved it (a person's login name), when someone did.
    pub approved_by: Option<String>,
    pub tainted: bool,
    pub reason: String,
    pub data: Value,
}

/// Ledger text is written by whoever proposed (the agent included) and printed to a terminal:
/// control characters (escape sequences, carriage returns, newlines), bidi overrides and invisible
/// characters could make one entry look like another, or hide it. Show them as visible escapes.
/// (The broker's `printable` does the same; keep the two in step.)
pub fn clean(s: &str) -> String {
    s.chars().map(|c| if hidden(c) { c.escape_unicode().to_string() } else { c.to_string() }).collect()
}

/// Characters that change what a terminal shows without being plainly visible themselves.
pub fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200b}'..='\u{200f}'     // zero-width space/joiners, LRM/RLM
            | '\u{2028}' | '\u{2029}'   // line/paragraph separators
            | '\u{202a}'..='\u{202e}'   // bidi embeddings and overrides
            | '\u{2060}'..='\u{2069}'   // word joiner, invisible operators, bidi isolates
            | '\u{061c}' | '\u{00ad}' | '\u{feff}'
            | '\u{fe00}'..='\u{fe0f}'   // variation selectors
            | '\u{3164}' | '\u{ffa0}' | '\u{2800}' | '\u{115f}' | '\u{1160}' // blank-looking letters
            | '\u{e0000}'..='\u{e007f}' // tag characters
        )
}

/// A one-line summary: cleaned, runs of whitespace collapsed, at most `max` characters — so padding
/// cannot push text onto what looks like a line of its own.
pub fn one_line(s: &str, max: usize) -> String {
    let s = clean(&s.split_whitespace().collect::<Vec<_>>().join(" "));
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}

/// One JSON object per line, as `torq-broker log --json` prints them.
pub fn parse(json_lines: &str) -> Result<Vec<Entry>> {
    json_lines
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let v: Value = serde_json::from_str(l).context("reading the ledger")?;
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            Ok(Entry {
                seq: v["seq"].as_u64().context("ledger entry without seq")?,
                ts: s("ts"),
                kind: s("kind"),
                tier: s("tier"),
                actor: s("actor"),
                approved_by: v["approved_by"].as_str().map(String::from),
                tainted: v["tainted"].as_bool().unwrap_or(false),
                reason: s("reason"),
                data: v["data"].clone(),
            })
        })
        .collect()
}

/// The entry that undid an applied change, if any.
fn undone_by(entries: &[Entry], applied: u64) -> Option<u64> {
    entries
        .iter()
        .find(|e| e.kind == "reverted" && e.data["applied"].as_u64() == Some(applied))
        .map(|e| e.seq)
}

/// The applied change to undo. `seq` may name the applied entry or the proposal it applied (both
/// are printed when a change lands); None means the latest change that is not undone yet.
pub fn undo_target(entries: &[Entry], seq: Option<u64>) -> Result<&Entry> {
    let applied = match seq {
        None => entries
            .iter()
            .rev()
            .find(|e| e.kind == "applied" && undone_by(entries, e.seq).is_none())
            .context("nothing to undo")?,
        Some(s) => {
            let e = entries.iter().find(|e| e.seq == s).with_context(|| format!("there is no #{s} in the ledger"))?;
            match e.kind.as_str() {
                "applied" => e,
                "proposal" => entries
                    .iter()
                    .find(|a| a.kind == "applied" && a.data["proposal"].as_u64() == Some(s))
                    .with_context(|| format!("proposal #{s} was never applied, so there is nothing to undo"))?,
                other => bail!("#{s} is '{other}', not a change that can be undone"),
            }
        }
    };
    if let Some(by) = undone_by(entries, applied.seq) {
        bail!("#{} ({}) was already undone by #{by}", applied.seq, one_line(&applied.reason, 80));
    }
    Ok(applied)
}

/// Now and the local offset from UTC, in seconds — passed in so rendering is a pure function.
pub struct Clock {
    pub now: i64,
    pub offset: i64,
}

impl Clock {
    /// The system clock, and the local offset as `date +%z` reports it (no time-zone database
    /// here; an entry from across a daylight-saving change can be shown an hour off).
    pub fn system() -> Self {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let z = std::process::Command::new("date").arg("+%z").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let offset = z.and_then(|z| parse_offset(&z)).unwrap_or(0);
        Clock { now, offset }
    }
}

/// "+0100", "-0430" → seconds.
pub fn parse_offset(z: &str) -> Option<i64> {
    let (sign, rest) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    if rest.len() != 4 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(sign * (rest[..2].parse::<i64>().ok()? * 3600 + rest[2..].parse::<i64>().ok()? * 60))
}

/// A broker time (UTC) in local time, as the ledger shows times: `2026-11-01 09:04:37`. Not a time
/// the broker wrote: as it is.
pub fn local_stamp(ts: &str, clock: &Clock) -> String {
    match parse_ts(ts) {
        Some(t) => {
            let l = t + clock.offset;
            format!("{} {:02}:{:02}:{:02}", crate::pkg::ymd(l.max(0) as u64), l.rem_euclid(86_400) / 3600, l.rem_euclid(3600) / 60, l.rem_euclid(60))
        }
        None => one_line(ts, 25),
    }
}

/// "2026-10-30T19:02:51Z" (how the broker writes times, in UTC) → Unix seconds.
pub fn parse_ts(ts: &str) -> Option<i64> {
    let n = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
    let (y, m, d, hh, mm, ss) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    // days from civil (the inverse of pkg::ymd)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Whether a proposal has a result (applied, failed, or its apply interrupted): then the log shows
/// only the result.
fn settled(entries: &[Entry], proposal: u64) -> bool {
    entries.iter().any(|e| match e.kind.as_str() {
        "applied" | "apply_failed" => e.data["proposal"].as_u64() == Some(proposal),
        "interrupted" => e.data["op"] == "apply" && e.data["target"].as_u64() == Some(proposal),
        _ => false,
    })
}

fn reason_of(entries: &[Entry], seq: Option<u64>) -> Option<&str> {
    let seq = seq?;
    entries.iter().find(|e| e.seq == seq).map(|e| e.reason.as_str())
}

/// Later changes, still in effect, that touched a file the given applied change touched: why it
/// cannot be undone on its own.
pub fn later_conflicts(entries: &[Entry], applied: u64) -> Vec<u64> {
    let files = |e: &Entry| -> Vec<String> {
        e.data["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).map(String::from).collect()
    };
    let Some(target) = entries.iter().find(|e| e.seq == applied) else { return vec![] };
    let mine = files(target);
    let touches = |e: &Entry| files(e).iter().any(|f| mine.contains(f));
    entries
        .iter()
        .filter(|e| e.seq > applied)
        .filter(|e| match e.kind.as_str() {
            // A later change still in effect.
            "applied" => undone_by(entries, e.seq).is_none() && touches(e),
            // A later undo of an earlier change edited the same files too (and cannot itself be
            // undone). The undo of a later change cancels that change, so it is no obstacle.
            "reverted" => e.data["applied"].as_u64().filter(|&a| a < applied).and_then(|a| entries.iter().find(|x| x.seq == a)).is_some_and(touches),
            _ => false,
        })
        .map(|e| e.seq)
        .collect()
}

/// The latest change that took effect (applied, undone, repaired), in the log's words.
pub fn last_change(entries: &[Entry]) -> Option<String> {
    let e = entries.iter().rev().find(|e| matches!(e.kind.as_str(), "applied" | "reverted" | "repaired"))?;
    Some(describe(entries, e).1)
}

/// One change as a log line: its symbol and what happened, in words.
fn describe(entries: &[Entry], e: &Entry) -> (&'static str, String) {
    match e.kind.as_str() {
        "applied" => ("✓", e.reason.clone()),
        "reverted" => ("↶", format!("Undid \"{}\"", e.reason.strip_prefix("Undo: ").unwrap_or(&e.reason))),
        "apply_failed" => {
            let what = reason_of(entries, e.data["proposal"].as_u64()).unwrap_or("a change");
            let after = if e.data["restored"] == false { "machine may not be as it was" } else { "nothing changed" };
            ("✗", format!("Failed: {what} ({after})"))
        }
        "revert_failed" => {
            let what = reason_of(entries, e.data["applied"].as_u64()).unwrap_or("a change");
            ("✗", format!("Undo failed: {what}"))
        }
        "interrupted" => ("⚠", format!("Interrupted: {}", reason_of(entries, e.data["target"].as_u64()).unwrap_or("a change"))),
        "init" => ("·", "TorqOS set up".to_string()),
        "repaired" if e.data["restored"] == false => ("✗", "Tried to return the machine to its history".to_string()),
        "repaired" => ("✓", "Returned the machine to its history".to_string()),
        "break_glass" => ("⚠", format!("Root shell opened: {}", clean(&e.reason))),
        "adopted" => ("✓", "This machine adopted the configuration".to_string()),
        "refused" => ("✗", format!("Refused: {}", clean(&e.reason))),
        "apply_refused" => ("✗", format!("Refused: {}", clean(&e.reason))),
        "remote_cleared" => ("✓", clean(&e.reason)),
        "data_restored" => ("↺", clean(&e.reason)),
        "snapshot_taken" | "snapshot_deleted" => ("✓", clean(&e.reason)),
        "snapshot_delete_failed" => ("✗", clean(&e.reason)),
        "ledger_mended" => ("⚠", clean(&e.reason)),
        "disagreement_acknowledged" => ("✓", clean(&e.reason)),
        "password" => ("✓", clean(&e.reason)),
        "password_failed" => ("✗", clean(&e.reason)),
        "proposal" => ("·", format!("Proposed, not applied: {}", e.reason)),
        other => ("·", other.to_string()),
    }
}

/// Who asked for a change. The broker writes failure and interruption entries itself, so for those
/// it is whoever asked for what failed (the proposal, or the change being undone) — an agent's
/// failed change must still say it was the agent's.
pub fn asked_by<'a>(entries: &'a [Entry], e: &'a Entry) -> &'a str {
    let pointed = match e.kind.as_str() {
        "apply_failed" => e.data["proposal"].as_u64(),
        "revert_failed" => e.data["applied"].as_u64(),
        "interrupted" => e.data["target"].as_u64(),
        _ => None,
    };
    pointed.and_then(|s| entries.iter().find(|x| x.seq == s)).map_or(e.actor.as_str(), |x| x.actor.as_str())
}

/// The last `limit` changes, oldest first, grouped by day in local time (docs/CLI-DESIGN.md).
/// A proposal that was applied (or failed) is folded into its result. Who made a change is shown
/// whenever it was not the user — the agent, say — and a change from a session that read
/// untrusted content is marked: the log is where a person audits the machine.
pub fn render(entries: &[Entry], limit: usize, ui: &crate::ui::Ui, clock: &Clock) -> String {
    let shown: Vec<&Entry> = entries.iter().filter(|e| !(e.kind == "proposal" && settled(entries, e.seq))).collect();
    let today = (clock.now + clock.offset).div_euclid(86_400);
    use crate::ui::{Col, Table};
    // One line per entry, cut to the window if need be: what an entry says cannot look like another
    // entry. Who made it in its own column, never cut.
    let mut t = Table::new(&[Col::Left(""), Col::Left(""), Col::Right(""), Col::Left(""), Col::Text("")]).indent(4).lines(1);
    let mut day = None;
    for e in &shown[shown.len().saturating_sub(limit)..] {
        let local = parse_ts(&e.ts).map(|t| t + clock.offset);
        let this_day = local.map(|t| t.div_euclid(86_400));
        if this_day != day {
            day = this_day;
            let label = match this_day {
                Some(d) if d == today => "Today".to_string(),
                Some(d) if d == today - 1 => "Yesterday".to_string(),
                Some(d) => crate::pkg::ymd((d * 86_400) as u64),
                None => "Unknown date".to_string(),
            };
            t.line(format!("  {}", ui.bold(&label)));
        }
        let time = local.map(|t| format!("{:02}:{:02}", t.rem_euclid(86_400) / 3600, t.rem_euclid(3600) / 60)).unwrap_or_else(|| "--:--".into());
        let (symbol, text) = describe(entries, e);
        // Who made it: the person who approved it, else who asked (an agent, the broker on its own).
        let mut who = match (&e.approved_by, asked_by(entries, e)) {
            (Some(p), _) => one_line(p, 16),
            (None, "user" | "") => String::new(),
            (None, a) => format!("asked by {}", one_line(a, 16)),
        };
        if e.tainted {
            who.push_str(" ⚠ tainted");
        }
        let symbol = match symbol {
            "✓" => ui.green(symbol),
            "✗" => ui.red(symbol),
            "⚠" => ui.yellow(symbol),
            s => s.to_string(),
        };
        // Its change number, as everywhere (`request system revert #85`).
        t.row(vec![time, symbol, format!("#{}", e.seq), who.trim_start().to_string(), one_line(&text, usize::MAX)]);
    }
    let mut out = t.render(ui);
    if shown.is_empty() {
        out.push_str("  Nothing has changed yet.\n");
    } else {
        out.push_str(&format!("\n{}\n", ui.dim(&ui.line("  ", "To undo one change: request system revert <change> (its number, like #85)."))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Vec<Entry> {
        let lines = [
            r#"{"seq":1,"ts":"2026-10-30T12:00:00Z","kind":"init","actor":"user","tier":"system","reason":"Config repository initialised","data":{}}"#,
            r#"{"seq":2,"ts":"2026-10-30T12:01:00Z","kind":"proposal","actor":"user","tier":"user","reason":"Install htop for viper","data":{}}"#,
            r#"{"seq":3,"ts":"2026-10-30T12:01:05Z","kind":"applied","actor":"user","tier":"user","reason":"Install htop for viper","data":{"proposal":2}}"#,
            r#"{"seq":4,"ts":"2026-10-30T12:02:00Z","kind":"proposal","actor":"user","tier":"system","reason":"Install jq system-wide","data":{}}"#,
            r#"{"seq":5,"ts":"2026-10-30T12:02:05Z","kind":"applied","actor":"user","tier":"system","reason":"Install jq system-wide","data":{"proposal":4}}"#,
            r#"{"seq":6,"ts":"2026-10-30T12:03:00Z","kind":"reverted","actor":"user","tier":"system","reason":"Undo: Install jq system-wide","data":{"applied":5}}"#,
            r#"{"seq":7,"ts":"2026-10-30T12:04:00Z","kind":"proposal","actor":"user","tier":"user","reason":"Install vim for viper","data":{}}"#,
        ];
        parse(&lines.join("\n")).unwrap()
    }

    #[test]
    fn undo_by_applied_or_proposal_number() {
        let l = ledger();
        assert_eq!(undo_target(&l, Some(3)).unwrap().seq, 3);
        assert_eq!(undo_target(&l, Some(2)).unwrap().seq, 3, "the proposal names its applied change");
    }

    #[test]
    fn undo_latest_skips_what_is_already_undone() {
        assert_eq!(undo_target(&ledger(), None).unwrap().seq, 3);
    }

    #[test]
    fn undo_refuses_what_cannot_be_undone() {
        let l = ledger();
        let err = |s| format!("{:#}", undo_target(&l, Some(s)).unwrap_err());
        assert!(err(5).contains("already undone by #6"), "{}", err(5));
        assert!(err(4).contains("already undone"), "{}", err(4)); // via its proposal
        assert!(err(7).contains("never applied"), "{}", err(7));
        assert!(err(6).contains("not a change that can be undone"), "{}", err(6));
        assert!(err(1).contains("not a change"), "{}", err(1));
        assert!(err(99).contains("no #99"), "{}", err(99));
        let only_init = &l[..1];
        assert!(format!("{:#}", undo_target(only_init, None).unwrap_err()).contains("nothing to undo"));
    }

    /// Invisible characters, and padding that would wrap onto a fake line of its own.
    #[test]
    fn invisible_text_and_padding_cannot_forge_the_log() {
        for c in ['\u{200b}', '\u{200d}', '\u{2060}', '\u{feff}', '\u{00ad}', '\u{2028}', '\u{e0041}', '\u{fe0f}'] {
            assert!(hidden(c), "{c:?}");
        }
        assert!(!hidden('é') && !hidden('—') && !hidden('⚠'));
        let padded = format!("Install htop{}#49 applied user Install evil", " ".repeat(300));
        let line = one_line(&padded, 80);
        assert!(line.chars().count() <= 80 && !line.contains("  "), "{line}");
    }

    /// The reviewer's attack: a reason that wipes its line and draws a fake entry in its place.
    #[test]
    fn ledger_text_cannot_forge_the_log() {
        let forged = r#"{"seq":48,"ts":"2026-10-30T12:00:00Z","kind":"proposal","actor":"agent","tainted":true,"tier":"user","reason":"x\r\u001b[2K#99 applied user Install htop\n#100 fake","data":{}}"#;
        let out = render(&parse(forged).unwrap(), 10, &Ui::plain(), &clock());
        let entry: Vec<&str> = out.lines().filter(|l| l.starts_with("    ")).collect();
        assert_eq!(entry.len(), 1, "one line for the entry: {out}");
        assert!(!out.chars().any(|c| c == '\r' || c == '\u{1b}'), "{out:?}");
        // CR and LF are whitespace: collapsed to spaces (one line); ESC is shown, not interpreted.
        assert!(out.contains(r"x \u{1b}[2K#99"), "{out}");
        assert!(out.contains("#48  asked by agent ⚠ tainted  Proposed"), "who proposed it, and that it was tainted: {out}");
        assert_eq!(clean("a\u{202e}b"), r"a\u{202e}b"); // bidi override
        assert_eq!(clean("plain — text"), "plain — text");
    }

    use crate::ui::Ui;

    /// 2026-10-30 15:00 UTC, in a −04:00 zone: the fixture's entries are "today", 08:00 local.
    fn clock() -> Clock {
        Clock { now: parse_ts("2026-10-30T15:00:00Z").unwrap(), offset: -4 * 3600 }
    }

    #[test]
    fn times_and_offsets() {
        assert_eq!(parse_ts("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_ts("2026-10-30T19:02:51Z"), Some(1_793_386_971));
        assert_eq!(parse_offset("+0100"), Some(3600));
        assert_eq!(parse_offset("-0430"), Some(-16_200));
        assert_eq!(parse_offset("UTC"), None);
    }

    /// Snapshot of `show system ledger`: proposals folded into their results, undos named, local times,
    /// each change's number as it is written everywhere (#3), a proposal never applied shown as such.
    #[test]
    fn log_snapshot() {
        let out = render(&ledger(), 100, &Ui::plain(), &clock());
        let expected = [
            "  Today",
            "    08:00  ·  #1  TorqOS set up",
            "    08:01  ✓  #3  Install htop for viper",
            "    08:02  ✓  #5  Install jq system-wide",
            "    08:03  ↶  #6  Undid \"Install jq system-wide\"",
            "    08:04  ·  #7  Proposed, not applied: Install vim for viper",
            "",
            "  To undo one change: request system revert <change> (its number, like #85).",
        ]
        .join("\n");
        assert_eq!(out.trim_end(), expected, "\n{out}");
        assert!(out.lines().all(|l| crate::ui::width(l) <= crate::ui::WIDTH), "80 columns");
        assert_eq!(render(&ledger(), 2, &Ui::plain(), &clock()).lines().filter(|l| l.starts_with("    ")).count(), 2);
        // A narrow terminal: each entry one line that fits it, cut (a reason cannot draw an entry).
        let narrow = render(&ledger(), 100, &Ui { fit: Some(30), ..Ui::plain() }, &clock());
        assert!(narrow.lines().all(|l| crate::ui::width(l) <= 30), "\n{narrow}");
        assert_eq!(narrow.lines().filter(|l| l.starts_with("    ")).count(), 5, "one line per entry:\n{narrow}");
        assert!(narrow.contains("\n    08:04  ·  #7  Proposed,…\n"), "\n{narrow}");
    }

    #[test]
    fn what_stands_in_the_way_of_an_undo() {
        let lines = [
            r#"{"seq":1,"ts":"2026-10-30T12:00:00Z","kind":"applied","actor":"user","tier":"user","reason":"Install a","data":{"files":["users/v/packages.json"]}}"#,
            r#"{"seq":2,"ts":"2026-10-30T12:01:00Z","kind":"applied","actor":"user","tier":"system","reason":"Enable ssh","data":{"files":["services.json"]}}"#,
            r#"{"seq":3,"ts":"2026-10-30T12:02:00Z","kind":"applied","actor":"user","tier":"user","reason":"Install b","data":{"files":["users/v/packages.json"]}}"#,
            r#"{"seq":4,"ts":"2026-10-30T12:03:00Z","kind":"applied","actor":"user","tier":"user","reason":"Install c","data":{"files":["users/v/packages.json"]}}"#,
            r#"{"seq":5,"ts":"2026-10-30T12:04:00Z","kind":"reverted","actor":"user","tier":"user","reason":"Undo: Install c","data":{"applied":4}}"#,
        ];
        let l = parse(&lines.join("\n")).unwrap();
        assert_eq!(later_conflicts(&l, 1), vec![3], "same file, in effect; not ssh, not 4 (applied and undone)");
        assert!(later_conflicts(&l, 3).is_empty());
        let mut more = lines.to_vec();
        more.push(r#"{"seq":6,"ts":"2026-10-30T12:05:00Z","kind":"reverted","actor":"user","tier":"user","reason":"Undo: Install a","data":{"applied":1}}"#);
        let l = parse(&more.join("\n")).unwrap();
        assert_eq!(later_conflicts(&l, 3), vec![6], "a later undo of an earlier change is in the way");
    }

    #[test]
    fn failures_name_the_change_not_the_broker() {
        let mut lines: Vec<String> = Vec::new();
        lines.push(r#"{"seq":8,"ts":"2026-10-30T12:05:00Z","kind":"proposal","actor":"agent","tier":"user","reason":"Install x","data":{}}"#.into());
        lines.push(r#"{"seq":9,"ts":"2026-10-30T12:05:05Z","kind":"apply_failed","actor":"broker","tier":"user","reason":"Rebuild failed; ...","data":{"proposal":8,"restored":true}}"#.into());
        let out = render(&parse(&lines.join("\n")).unwrap(), 10, &Ui::plain(), &clock());
        assert!(out.contains("#9  asked by agent  Failed: Install x (nothing changed)"), "an agent's failure stays the agent's:\n{out}");
        assert!(!out.contains("by broker"), "{out}");
        lines.push(r#"{"seq":10,"ts":"2026-10-30T12:06:00Z","kind":"proposal","actor":"agent","tier":"user","reason":"Install y","data":{}}"#.into());
        lines.push(r#"{"seq":11,"ts":"2026-10-30T12:07:00Z","kind":"interrupted","actor":"broker","tier":"user","reason":"...","data":{"op":"apply","target":10}}"#.into());
        let out = render(&parse(&lines.join("\n")).unwrap(), 10, &Ui::plain(), &clock());
        assert!(out.contains("#11  asked by agent  Interrupted: Install y") && !out.contains("Proposed, not applied"), "{out}");
    }

}
