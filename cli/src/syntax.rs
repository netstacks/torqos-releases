//! torqcli's commands as one model (`Op`, ADR-016, ADR-021). The grammar (grammar.rs, ADR-025) reads
//! a line into an `Op`; the session executes `Op`s. A property test checks that every `Op`, rendered
//! as it is typed, reads back to itself.

use crate::session::Mode;
use crate::filter::Filter;
use anyhow::Result;

/// A word with a meaning of its own after a path (`set <place> config <file>`): no list entry may be
/// named it (settings.rs, tree.rs), so a line never means two things.
pub const RESERVED: &[&str] = &["config"];

/// `| display …` after a `show`: what each view offers (CLI-DESIGN principle 5: internals only when
/// asked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// A configuration as `set` commands.
    Set,
    /// With what is behind it: the files, commits, store paths.
    Detail,
    /// A comparison as the raw diff of the files (what the broker records).
    Diff,
}

impl Display {
    pub fn name(self) -> &'static str {
        match self {
            Display::Set => "set",
            Display::Detail => "detail",
            Display::Diff => "diff",
        }
    }
}

/// `commit` options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitOptions {
    pub check: bool,
    pub confirm: Option<u32>,
    pub comment: Option<String>,
    /// `and-quit`: leave configuration mode once committed.
    pub and_quit: bool,
}

/// One command. Paths are relative to where configuration mode is (`edit`), except in operational
/// mode, where they are from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    // Operational mode.
    Configure,
    /// `show configuration [path] [| display set|detail]`.
    ShowActive { path: Vec<String>, display: Option<Display> },
    ShowCommits { display: Option<Display> },
    ShowRollback { n: usize, compare: Option<usize>, display: Option<Display> },
    ShowAlarms { display: Option<Display> },
    /// The proposals waiting for a person (the agent's, ADR-041).
    ShowProposals,
    /// The configuration's remote (ADR-036).
    ShowRemote,
    ShowSnapshots,
    ShowServices,
    /// `show system ledger [<count>]`: the ledger itself — every change, refused and failed ones too.
    ShowLedger { count: Option<usize> },
    ShowPackages { terms: Vec<String> },
    /// `show version`, `show system uptime`, `show interfaces`, `show system storage`, `show users`:
    /// the machine.
    ShowVersion,
    ShowUptime,
    ShowInterfaces { name: Option<String> },
    ShowStorage,
    ShowUsers,
    Request(Request),
    /// `file show|edit|delete <path>`, `file list`: the config repo's config files (in configuration
    /// mode, the candidate's; in operational mode, the active ones; `edit` and `delete` only there).
    File { action: FileAction, path: Option<String> },
    /// `save <file>`: the whole candidate, and its config files' content, to a file (saved.rs).
    Save { file: String },
    /// `load override|merge|set <file>|terminal`: a saved configuration into the candidate, all or
    /// nothing.
    Load { how: LoadHow, from: LoadFrom },
    // Configuration mode.
    /// A leaf's value or leaf-list elements: the words after `set`.
    Set { words: Vec<String> },
    /// `delete <path>`: a setting, a value of a list, or everything below a place.
    Delete { words: Vec<String> },
    /// `edit <path>`: go down to a container or list entry.
    Edit { words: Vec<String> },
    /// `up [<n>]`: up n levels (1).
    Up { n: usize },
    /// `top [<command>]`: to the top, or run a command from the top and come back.
    Top { then: Option<Box<Op>> },
    /// `show [path]`: the candidate.
    ShowCandidate { path: Vec<String>, display: Option<Display> },
    /// `show | compare`: what the candidate changes.
    /// `show | compare [rollback <n>]`: what the candidate changes from its base, or from
    /// configuration n.
    Compare { with: Option<usize>, display: Option<Display> },
    /// `rollback [<n> [<path>]]`: configuration n into the candidate — or only the place `path`,
    /// put back as configuration n has it (`rollback 0 networking hostName` undoes one change).
    Rollback { n: usize, path: Vec<String> },
    Commit(CommitOptions),
    /// `run <command>`: an operational command from configuration mode.
    Run(Box<Op>),
    /// `exit`: up one level, or out of configuration mode at the top; in operational mode, leave
    /// torqcli.
    Exit,
    /// `exit configuration-mode`: out of configuration mode from anywhere.
    End,
    /// `help [<command> | <words>]`: how to use torqcli, one command, or a search of the commands
    /// and settings.
    Help { topic: Vec<String> },
}

/// `request system …`: what the machine is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Repair,
    /// A person's password (`None`: the one asking).
    Password { user: Option<String> },
    /// Undo one change (a new change forward).
    Revert { change: u64 },
    /// Apply a proposal someone else made (the agent's: ADR-041), after showing it and asking.
    Apply { proposal: u64 },
    /// Say no more of where the ledger and the repository disagree, on the record.
    LedgerAcknowledge,
    /// A snapshot of the data now (ADR-038).
    SnapshotTake,
    /// The snapshot of a change gone (ADR-038).
    SnapshotDelete { change: u64 },
    /// Put a path back as it was before a change (ADR-038).
    RestoreData { change: u64, path: String },
    /// This machine's key for the remote (ADR-036), made the first time.
    RemoteKey,
    /// Clear the remote's alarm (ADR-036), ledgered.
    RemoteClear,
    /// The remote's incoming configuration into the candidate (ADR-036).
    Pull,
    /// The newest TorqOS (`all`: every input) into the candidate.
    Update { all: bool },
    /// An earlier configuration's software into the candidate.
    SoftwareRollback { n: usize },
}

/// How `load` takes a saved configuration: as the whole candidate, onto it, or as `set` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadHow {
    Override,
    Merge,
    Set,
}

/// Where `load` reads from: a file, or what is pasted at the terminal (`terminal`, ended by Ctrl-D;
/// a file of that name is `./terminal`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadFrom {
    File(String),
    Terminal,
}

/// What `file` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Show,
    Edit,
    Delete,
    List,
}

/// The highest rollback number (how far back `rollback` and `show system rollback` reach).
pub const MAX_ROLLBACK: usize = 10_000;


/// Read one line: the command and the filters after it (the grammar's, grammar.rs).
pub fn parse_line(mode: Mode, w: &[String]) -> Result<(Op, Vec<Filter>)> {
    crate::grammar::read_line(mode, w).map(|l| (l.op, l.filters))
}

// Rendering (Op → text) is what the round-trip test checks the parser against; nothing else prints
// an Op yet, so it is compiled for the tests only.

#[cfg(test)]
fn display_text(d: &Option<Display>) -> String {
    d.map(|d| format!(" | display {}", d.name())).unwrap_or_default()
}

#[cfg(test)]
fn join(w: &[String]) -> String {
    w.iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ")
}

/// A word as it must be typed: quoted if it has a space or a character the line syntax uses.
pub fn quote(w: &str) -> String {
    if w.is_empty() || w.chars().any(|c| c.is_whitespace() || "\"'|;\\".contains(c)) {
        format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        w.to_string()
    }
}

#[cfg(test)]
fn sep(prefix: &str, w: &[String]) -> String {
    if w.is_empty() { prefix.to_string() } else { format!("{prefix} {}", join(w)) }
}

/// How `op` is typed. The inverse of `parse` (tested).
#[cfg(test)]
pub fn render(op: &Op) -> String {
    let commit = |o: &CommitOptions| {
        let mut s = String::from("commit");
        if o.check {
            s.push_str(" check");
        }
        if let Some(m) = o.confirm {
            s.push_str(&format!(" confirmed {m}"));
        }
        if let Some(c) = &o.comment {
            s.push_str(&format!(" comment {}", quote(c)));
        }
        if o.and_quit {
            s.push_str(" and-quit");
        }
        s
    };
    match op {
        Op::Configure => "configure".into(),
        Op::ShowActive { path, display } => format!("{}{}", sep("show configuration", path), display_text(display)),
        Op::ShowCommits { display } => format!("show system commit{}", display_text(display)),
        Op::ShowRollback { n, compare: None, display } => format!("show system rollback {n}{}", display_text(display)),
        Op::ShowRollback { n, compare: Some(m), display } => format!("show system rollback {n} compare {m}{}", display_text(display)),
        Op::ShowAlarms { display } => format!("show system alarms{}", display_text(display)),
        Op::ShowProposals => "show system proposals".into(),
        Op::ShowRemote => "show system remote".into(),
        Op::ShowSnapshots => "show system snapshots".into(),
        Op::ShowServices => "show services".into(),
        Op::ShowLedger { count: None } => "show system ledger".into(),
        Op::ShowLedger { count: Some(n) } => format!("show system ledger {n}"),
        Op::ShowPackages { terms } => sep("show packages", terms),
        Op::ShowVersion => "show version".into(),
        Op::ShowUptime => "show system uptime".into(),
        Op::ShowInterfaces { name: None } => "show interfaces".into(),
        Op::ShowInterfaces { name: Some(n) } => format!("show interfaces {}", quote(n)),
        Op::ShowStorage => "show system storage".into(),
        Op::ShowUsers => "show users".into(),
        Op::Request(r) => match r {
            Request::Repair => "request system repair".into(),
            Request::Password { user: None } => "request system password".into(),
            Request::Password { user: Some(u) } => format!("request system password {}", quote(u)),
            Request::Revert { change } => format!("request system revert {change}"),
            Request::Apply { proposal } => format!("request system apply {proposal}"),
            Request::RestoreData { change, path } => format!("request system snapshot restore {change} {path}"),
            Request::SnapshotTake => "request system snapshot take".into(),
            Request::SnapshotDelete { change } => format!("request system snapshot delete {change}"),
            Request::LedgerAcknowledge => "request system ledger acknowledge".into(),
            Request::RemoteKey => "request system remote key".into(),
            Request::RemoteClear => "request system remote clear".into(),
            Request::Pull => "request system pull".into(),
            Request::Update { all: false } => "request system update".into(),
            Request::Update { all: true } => "request system update all".into(),
            Request::SoftwareRollback { n } => format!("request system software rollback {n}"),
        },
        Op::Save { file } => format!("save {}", quote(file)),
        Op::Load { how, from } => {
            let h = match how {
                LoadHow::Override => "override",
                LoadHow::Merge => "merge",
                LoadHow::Set => "set",
            };
            match from {
                LoadFrom::File(f) => format!("load {h} {}", quote(f)),
                LoadFrom::Terminal => format!("load {h} terminal"),
            }
        }
        Op::File { action, path } => {
            let a = match action {
                FileAction::Show => "show",
                FileAction::Edit => "edit",
                FileAction::Delete => "delete",
                FileAction::List => "list",
            };
            sep(&format!("file {a}"), &path.iter().cloned().collect::<Vec<_>>())
        }
        Op::Set { words } => sep("set", words),
        Op::Delete { words } => sep("delete", words),
        Op::Edit { words } => sep("edit", words),
        Op::Up { n: 1 } => "up".into(),
        Op::Up { n } => format!("up {n}"),
        Op::Top { then: None } => "top".into(),
        Op::Top { then: Some(op) } => format!("top {}", render(op)),
        Op::ShowCandidate { path, display } => format!("{}{}", sep("show", path), display_text(display)),
        Op::Compare { with: None, display } => format!("show | compare{}", display_text(display)),
        Op::Compare { with: Some(n), display } => format!("show | compare rollback {n}{}", display_text(display)),
        Op::Rollback { n, path } => sep(&format!("rollback {n}"), path),
        Op::Commit(o) => commit(o),
        Op::Run(op) => format!("run {}", render(op)),
        Op::Exit => "exit".into(),
        Op::End => "exit configuration-mode".into(),
        Op::Help { topic } => sep("help", topic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<String> {
        crate::session::words(s).unwrap()
    }
    fn p(mode: Mode, line: &str) -> Op {
        parse_line(mode, &w(line)).unwrap_or_else(|e| panic!("{line}: {e:#}")).0
    }

    /// Every operation, in both modes it exists in.
    fn ops() -> Vec<(Mode, Op)> {
        let v = |s: &str| w(s);
        let (set, detail, diff) = (Some(Display::Set), Some(Display::Detail), Some(Display::Diff));
        vec![
            (Mode::Operational, Op::Configure),
            (Mode::Operational, Op::ShowActive { path: vec![], display: None }),
            (Mode::Operational, Op::ShowActive { path: v("users dev"), display: set }),
            (Mode::Operational, Op::ShowActive { path: v("system"), display: detail }),
            (Mode::Operational, Op::ShowCommits { display: None }),
            (Mode::Operational, Op::ShowCommits { display: detail }),
            (Mode::Operational, Op::ShowRollback { n: 3, compare: None, display: None }),
            (Mode::Operational, Op::ShowRollback { n: 3, compare: None, display: detail }),
            (Mode::Operational, Op::ShowRollback { n: 0, compare: Some(2), display: None }),
            (Mode::Operational, Op::ShowRollback { n: 0, compare: Some(2), display: diff }),
            (Mode::Operational, Op::ShowAlarms { display: None }),
            (Mode::Operational, Op::ShowProposals),
            (Mode::Operational, Op::ShowRemote),
            (Mode::Operational, Op::ShowSnapshots),
            (Mode::Operational, Op::ShowAlarms { display: detail }),
            (Mode::Operational, Op::ShowServices),
            (Mode::Operational, Op::ShowVersion),
            (Mode::Operational, Op::ShowUptime),
            (Mode::Operational, Op::ShowInterfaces { name: None }),
            (Mode::Operational, Op::ShowInterfaces { name: Some("enp0s1".into()) }),
            (Mode::Operational, Op::ShowStorage),
            (Mode::Operational, Op::ShowUsers),
            (Mode::Operational, Op::ShowLedger { count: None }),
            (Mode::Operational, Op::ShowLedger { count: Some(5) }),
            (Mode::Operational, Op::ShowPackages { terms: v("rip grep") }),
            (Mode::Operational, Op::Request(Request::Repair)),
            (Mode::Operational, Op::Request(Request::Password { user: None })),
            (Mode::Operational, Op::Request(Request::Password { user: Some("dana".into()) })),
            (Mode::Operational, Op::Request(Request::Revert { change: 12 })),
            (Mode::Operational, Op::Request(Request::Apply { proposal: 12 })),
            (Mode::Operational, Op::Request(Request::RestoreData { change: 12, path: "/home/alice/a".into() })),
            (Mode::Operational, Op::Request(Request::SnapshotTake)),
            (Mode::Operational, Op::Request(Request::SnapshotDelete { change: 12 })),
            (Mode::Operational, Op::Request(Request::LedgerAcknowledge)),
            (Mode::Operational, Op::Request(Request::RemoteKey)),
            (Mode::Operational, Op::Request(Request::RemoteClear)),
            (Mode::Operational, Op::Request(Request::Pull)),
            (Mode::Operational, Op::Request(Request::Update { all: true })),
            (Mode::Operational, Op::Request(Request::SoftwareRollback { n: 2 })),
            (Mode::Operational, Op::Exit),
            (Mode::Configuration, Op::Set { words: v("system host-name torq") }),
            (Mode::Configuration, Op::Set { words: v("system packages [ htop vim ]") }),
            (Mode::Configuration, Op::Set { words: v("users dev packages jq") }),
            (Mode::Configuration, Op::Delete { words: v("system packages htop") }),
            (Mode::Configuration, Op::Delete { words: v("system") }),
            (Mode::Configuration, Op::Delete { words: vec![] }),
            (Mode::Configuration, Op::Edit { words: v("system") }),
            (Mode::Configuration, Op::Edit { words: v("users dev") }),
            (Mode::Configuration, Op::Up { n: 1 }),
            (Mode::Configuration, Op::Up { n: 3 }),
            (Mode::Configuration, Op::Top { then: None }),
            (Mode::Configuration, Op::Top { then: Some(Box::new(Op::Set { words: v("time timeZone UTC") })) }),
            (Mode::Configuration, Op::Top { then: Some(Box::new(Op::ShowCandidate { path: v("networking"), display: None })) }),
            (Mode::Configuration, Op::ShowCandidate { path: vec![], display: None }),
            (Mode::Configuration, Op::ShowCandidate { path: v("system"), display: set }),
            (Mode::Configuration, Op::ShowCandidate { path: vec![], display: detail }),
            (Mode::Configuration, Op::Compare { with: None, display: None }),
            (Mode::Configuration, Op::Compare { with: None, display: diff }),
            (Mode::Configuration, Op::Compare { with: Some(3), display: None }),
            (Mode::Configuration, Op::Rollback { n: 2, path: vec![] }),
            (Mode::Configuration, Op::Rollback { n: 0, path: v("networking hostName") }),
            (Mode::Configuration, Op::Save { file: "/home/dev/my device.conf".into() }),
            (Mode::Configuration, Op::Load { how: LoadHow::Override, from: LoadFrom::File("a.conf".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Merge, from: LoadFrom::File("a.conf".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Set, from: LoadFrom::File("a.set".into()) }),
            (Mode::Configuration, Op::Load { how: LoadHow::Merge, from: LoadFrom::Terminal }),
            (Mode::Configuration, Op::File { action: FileAction::Edit, path: Some("custom/a.nix".into()) }),
            (Mode::Configuration, Op::File { action: FileAction::List, path: None }),
            (Mode::Operational, Op::File { action: FileAction::Show, path: Some("a.nix".into()) }),
            (Mode::Configuration, Op::Run(Box::new(Op::File { action: FileAction::List, path: None }))),
            (Mode::Configuration, Op::Top { then: Some(Box::new(Op::Run(Box::new(Op::ShowServices)))) }),
            (Mode::Configuration, Op::Commit(CommitOptions::default())),
            (Mode::Configuration, Op::Commit(CommitOptions { check: true, ..Default::default() })),
            (Mode::Configuration, Op::Commit(CommitOptions { check: false, confirm: Some(5), comment: Some("ssh; keys | only \"x\"".into()), and_quit: false })),
            (Mode::Configuration, Op::Commit(CommitOptions { and_quit: true, ..Default::default() })),
            (Mode::Configuration, Op::Commit(CommitOptions { check: false, confirm: None, comment: Some("why".into()), and_quit: true })),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowAlarms { display: None }))),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowActive { path: v("system"), display: None }))),
            (Mode::Configuration, Op::Run(Box::new(Op::ShowRollback { n: 1, compare: Some(0), display: diff }))),
            (Mode::Configuration, Op::Exit),
            (Mode::Configuration, Op::End),
            (Mode::Operational, Op::Help { topic: vec![] }),
            (Mode::Configuration, Op::Help { topic: v("show system") }),
        ]
    }

    /// Every operation, rendered as it is typed, parses back to itself.
    #[test]
    fn every_op_round_trips() {
        for (mode, op) in ops() {
            let line = render(&op);
            assert_eq!(p(mode, &line), op, "{line:?}");
        }
    }

    /// One syntax: a line is a command, never a bare path; the other syntax's words are not words.
    #[test]
    fn commands_only() {
        for (mode, line, want) in [
            (Mode::Configuration, "system host-name x", "'system' is not a word here (one of: commit, delete,"),
            (Mode::Configuration, "no system packages jq", "'no' is not a word here"),
            (Mode::Configuration, "do show system alarms", "'do' is not a word here"),
            (Mode::Configuration, "end", "'end' is not a word here"),
            (Mode::Operational, "show running-config", "'running-config' is not a word here (after `show`)"),
            (Mode::Operational, "switch cli", "'switch' is not a word here"),
            (Mode::Operational, "set system host-name x", "'set' is a configuration-mode command: `configure` first"),
            (Mode::Configuration, "request system repair", "'request' is an operational-mode command: `run request …` runs it from here"),
            (Mode::Configuration, "run configure", "'configure' is not a word here (after `run`)"),
            (Mode::Configuration, "top exit", "'exit' is not a word here (after `top`)"),
            (Mode::Configuration, "top top show", "'top' is not a word here (after `top`) (one of: commit, delete, edit"),
            (Mode::Operational, "exit now", "'now' is not a word here: nothing may follow `exit`"),
            (Mode::Operational, "show packages", "needs <word> (words to search for, in names and descriptions)"),
            (Mode::Operational, "show system", "not a whole command: one of these may follow: alarms, commit, ledger, proposals, remote, rollback, snapshots, storage, uptime"),
            (Mode::Operational, "request system revert x", "'x' is not a number (change numbers: 1, 2, …)"),
            (Mode::Configuration, "up 0", "'0' is out of range: levels go from 1 to 10000"),
            (Mode::Configuration, "commit check comment x", "'comment' is not a word here: nothing may follow `commit check`"),
            (Mode::Configuration, "commit comment a comment b", "'comment' is not a word here (after `commit comment a`)"),
        ] {
            let e = format!("{:#}", parse_line(mode, &w(line)).unwrap_err());
            assert!(e.contains(want), "{line}: {e}");
        }
        // Abbreviations: a unique beginning is the word; where a value may stand, the word is the value.
        assert_eq!(p(Mode::Operational, "sh sys ala"), Op::ShowAlarms { display: None });
        assert_eq!(p(Mode::Configuration, "load merge term"), Op::Load { how: LoadHow::Merge, from: LoadFrom::File("term".into()) });
        assert_eq!(p(Mode::Configuration, "com con 5 and"), Op::Commit(CommitOptions { confirm: Some(5), and_quit: true, ..Default::default() }));
    }

    fn pl(mode: Mode, line: &str) -> Result<(Op, Vec<Filter>)> {
        parse_line(mode, &w(line))
    }
    fn err(mode: Mode, line: &str) -> String {
        format!("{:#}", pl(mode, line).unwrap_err())
    }
    fn f(tool: &str, args: &str) -> Filter {
        Filter { tool: tool.into(), args: w(args) }
    }

    /// Every show, with filters after it, parses back to itself and its filters.
    #[test]
    fn filters_follow_every_show() {
        let filters = vec![f("grep", "-Ei \"host|name\""), f("sort", "-r"), f("wc", "-l")];
        let mut n = 0;
        for (mode, op) in ops().into_iter().filter(|(mode, op)| crate::grammar::pipe_help(*mode, &w(&render(op))).is_some()) {
            let line = format!("{} | grep -Ei \"host|name\" | sort -r | wc -l", render(&op));
            assert_eq!(pl(mode, &line).unwrap_or_else(|e| panic!("{line}: {e:#}")), (op.clone(), filters.clone()), "{line}");
            n += 1;
        }
        assert!(n >= 8, "the shows are all there ({n})");
    }

    #[test]
    fn each_view_has_its_own_displays() {
        let (o, c) = (Mode::Operational, Mode::Configuration);
        for (mode, line, want) in [
            (o, "show system commit | display set", "'set' is not a view (after `| display`) (one of: detail)"),
            (o, "show system alarms | display diff", "'diff' is not a view (after `| display`) (one of: detail)"),
            (o, "show configuration | display diff", "'diff' is not a view (after `| display`) (one of: detail, set)"),
            (o, "show system rollback 1 compare 0 | display set", "'set' is not a view (after `| display`) (one of: diff)"),
            (o, "show configuration | display set | display detail", "'display' is not a word here (after `show configuration | display set |`)"),
            (o, "show system commit extra", "'extra' is not a word here: nothing may follow `show system commit`"),
            (o, "show system rollback 1 compare", "needs <number> (the configuration to compare with)"),
            (c, "show | compare | display set", "'set' is not a view (after `| display`) (one of: diff)"),
            (c, "show | display diff", "'diff' is not a view (after `| display`) (one of: detail, set)"),
            (o, "show system commit | count", "'count' is not a word here (after `show system commit |`): lines are counted with `| wc -l`"),
            (o, "show services | match ssh", "lines are matched with `| grep <pattern>`"),
        ] {
            let e = err(mode, line);
            assert!(e.contains(want), "{line}: {e}");
        }
    }

    #[test]
    fn filters_come_after_the_commands_own_pipes() {
        let (o, c) = (Mode::Operational, Mode::Configuration);
        assert_eq!(pl(o, "show configuration | grep 'a b'").unwrap().1, [f("grep", "\"a b\"")]);
        assert_eq!(pl(o, "show configuration | grep \"a|b\"").unwrap().1, [f("grep", "\"a|b\"")], "a quoted | is the pattern's");
        assert_eq!(pl(c, "show | compare | grep x").unwrap(), (Op::Compare { with: None, display: None }, vec![f("grep", "x")]));
        assert_eq!(pl(c, "show | comp roll 2 | display diff").unwrap().0, Op::Compare { with: Some(2), display: Some(Display::Diff) }, "abbreviated, as everywhere");
        for (line, want) in [
            ("show | compare 2", "'2' is not a word here (after `show | compare`) (one of: rollback)"),
            ("show | compare rollback", "| compare rollback needs <number>"),
            ("show | compare rollback x", "'x' is not a number"),
            ("show | compare rollback 1 2", "'2' is not a word here: nothing may follow `show | compare rollback 1`"),
        ] {
            assert!(err(c, line).contains(want), "{line}: {}", err(c, line));
        }
        let (op, fs) = pl(c, "show system | display set | wc -l").unwrap();
        assert_eq!((op, fs), (Op::ShowCandidate { path: w("system"), display: Some(Display::Set) }, vec![f("wc", "-l")]));
        assert_eq!(pl(c, "run show system alarms | wc -l").unwrap().0, Op::Run(Box::new(Op::ShowAlarms { display: None })));
        assert_eq!(pl(c, "show").unwrap().1, [], "no pipe, no filter");
        for (line, want) in [
            ("show configuration | grep a | display set", "| display must come before the filters"),
            ("show configuration | grep x |", "a | needs something after it (one of: cut, grep"),
            ("show configuration |", "a | needs something after it (one of: display, cut, grep"),
            ("show configuration | sed s/a/b/", "'sed' is not a word here (after `show configuration |`) (one of: display, cut, grep"),
            ("show configuration | sh -c x", "'sh' is not a word here"),
            ("show configuration | grep", "| grep needs <pattern>, as in `| grep host`"),
        ] {
            let e = err(o, line);
            assert!(e.contains(want), "{line}: {e}");
        }
        assert_eq!(pl(o, "show packages rg | grep x").unwrap(), (Op::ShowPackages { terms: w("rg") }, vec![f("grep", "x")]), "a search is a show like any other");
        assert_eq!(pl(o, "file show a.nix | head").unwrap().0, Op::File { action: FileAction::Show, path: Some("a.nix".into()) }, "a file shown is output too");
        for line in ["set system host-name a | wc -l", "commit | grep x", "rollback 1 | head"] {
            let e = err(c, line);
            assert!(e.contains("prints nothing to filter"), "{line}: {e}");
        }
        let many = format!("show configuration{}", " | wc -l".repeat(16));
        assert!(pl(o, &many).is_ok());
        let e = err(o, &format!("{many} | wc -l"));
        assert!(e.contains("at most 16 filters"), "{e}");
    }
}
