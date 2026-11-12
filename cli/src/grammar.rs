//! The command language, declared once (ADR-025): each command's words, help, values, the pipes
//! its output takes, and how a whole line becomes an `Op`. One walk over it parses a line, expands
//! abbreviations, lists what may come next (`?`, Tab) and says why a line is refused, so these can
//! never disagree. Configuration paths are the tree's (complete.rs, whole.rs); filters, filter.rs's.

use crate::session::Mode;
use crate::syntax::{CommitOptions, Display, FileAction, LoadFrom, LoadHow, Op, Request};
use anyhow::{bail, Result};

/// The modes a command is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum In {
    Op,
    Cf,
    Both,
}

impl In {
    fn has(self, m: Mode) -> bool {
        matches!((self, m), (In::Both, _) | (In::Op, Mode::Operational) | (In::Cf, Mode::Configuration))
    }
}

/// A value at one place of a command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Value {
    /// A configuration number (0 is the current one).
    Rollback,
    /// Levels to go up.
    Levels,
    /// How many entries.
    Count,
    /// A change number (the ledger's).
    Change,
    /// Minutes to confirm a commit in.
    Minutes,
    /// When a commit is applied (`commit at`): HH:MM, "YYYY-MM-DD HH:MM", RFC 3339.
    Time,
    /// A person's account.
    User,
    /// A file anywhere (save, load: in the home folder if relative).
    File,
    /// A Nix file of the configuration (`file show|edit|delete`).
    NixFile,
    /// A network interface of the machine (`show interfaces <interface>`).
    Interface,
    /// A declared backup's name (`services restic backups <name>`).
    Backup,
    /// A backup's snapshot (`latest`, or its id) — or, where it may be left out, the path after it.
    Snapshot,
    /// A secret's name (ADR-052).
    Secret,
    /// A host a secret may be used for: a DNS name.
    Host,
    /// Free text (a comment).
    Text,
}

impl Value {
    pub fn placeholder(self) -> &'static str {
        match self {
            Value::Rollback => "<number>",
            Value::Levels => "<n>",
            Value::Count => "<count>",
            Value::Change => "<change>",
            Value::Minutes => "<minutes>",
            Value::Time => "<time>",
            Value::User => "<user>",
            Value::File => "<file>",
            Value::NixFile => "<nix-file>",
            Value::Interface => "<interface>",
            Value::Backup => "<backup>",
            Value::Snapshot => "<snapshot>",
            Value::Secret => "<secret>",
            Value::Host => "<host>",
            Value::Text => "<\"text\">",
        }
    }

    /// Whether `w` is such a value, and if not, why.
    pub fn check(self, w: &str) -> Result<()> {
        let whole = |w: &str, what: &str, low: u64, high: u64| -> Result<u64> {
            match w.parse::<u64>() {
                Ok(n) if (low..=high).contains(&n) => Ok(n),
                Ok(_) => bail!("'{w}' is out of range: {what} go from {low} to {high}"),
                Err(_) => bail!("'{w}' is not a number ({what}: {low}, {}, …)", low + 1),
            }
        };
        match self {
            Value::Rollback => whole(w, "configuration numbers", 0, crate::syntax::MAX_ROLLBACK as u64).map(drop),
            Value::Levels => whole(w, "levels", 1, 10_000).map(drop),
            Value::Count => whole(w, "counts", 1, 10_000).map(drop),
            // As the ledger writes it (#85) or not.
            Value::Change => whole(w.strip_prefix('#').unwrap_or(w), "change numbers", 1, u64::MAX).map(drop),
            Value::Minutes => whole(w, "minutes", 1, 1440).map(drop),
            Value::Time => crate::at::form(w),
            Value::User if !w.is_empty() => Ok(()),
            Value::File | Value::NixFile | Value::Interface if !w.is_empty() => Ok(()),
            // As modules/backup.nix asserts a name is (it names its key's file).
            Value::Backup if (1..=64).contains(&w.len()) && w.starts_with(|c: char| c.is_ascii_alphanumeric()) && w.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => Ok(()),
            Value::Backup => bail!("'{w}' is not a backup's name (letters, digits, - and _: show system backups lists them)"),
            Value::Snapshot if w == "latest" || w.starts_with('/') || ((8..=64).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))) => Ok(()),
            Value::Snapshot => bail!("'{w}' is neither a snapshot (latest, or an id from show system backups) nor a path"),
            Value::Secret if torq_wire::secrets::name_ok(w) => Ok(()),
            Value::Secret => bail!("'{w}' is not a secret's name (letters, digits, - and _: show system secrets lists them)"),
            Value::Host if torq_wire::secrets::host_ok(w) => Ok(()),
            Value::Host => bail!("'{w}' is not a host name (lowercase, with a dot: api.github.com; no address, no wildcard)"),
            Value::Text => Ok(()),
            _ => bail!("an empty word is not a {}", self.placeholder()),
        }
    }
}

/// What stands at one place of a command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Word {
    /// The word itself.
    Fixed(&'static str),
    /// One value.
    Value(Value),
    /// A configuration path: every word to the end (or a `|`), the tree's to read.
    Path,
    /// One or more words to the end (or a `|`): search words.
    Terms,
    /// A whole command of a mode, as from where that mode starts (`run`, `top`).
    Command(Mode),
}

/// What a command's output takes after a `|`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pipes {
    /// Nothing: it prints no output to filter.
    None,
    /// Filters (grep, wc, …).
    Filters,
    /// Filters, and these `| display` views.
    Views(&'static [Display]),
    /// A command that does something as it goes (`commit`, `request system update`): `| display
    /// detail` alone — each step shown as it happens. No filters: what it prints is not data.
    Steps,
    /// The candidate's `show`: `| compare`, `| display set|detail` (after compare: `| display diff`),
    /// and filters.
    Candidate,
}

/// How a whole line becomes an `Op`, from what was read.
pub type Build = fn(&Read) -> Result<Op>;

/// One place of a command.
pub struct Node {
    pub word: Word,
    pub help: &'static str,
    pub mode: In,
    /// Only where its mode starts, not after `run` or `top` (configure, exit, quit).
    pub top_only: bool,
    /// May appear once in a line (commit's options).
    pub once: bool,
    /// The line may end here: how it becomes an `Op`.
    pub whole: Option<Build>,
    pub pipes: Pipes,
    /// What may follow.
    pub then: &'static [Node],
}

const fn node(word: Word, help: &'static str, mode: In) -> Node {
    Node { word, help, mode, top_only: false, once: false, whole: None, pipes: Pipes::None, then: &[] }
}
const fn fixed(w: &'static str, help: &'static str, mode: In) -> Node {
    node(Word::Fixed(w), help, mode)
}
const fn value(v: Value, help: &'static str) -> Node {
    node(Word::Value(v), help, In::Both)
}

impl Node {
    const fn whole(mut self, b: Build) -> Self {
        self.whole = Some(b);
        self
    }
    const fn then(mut self, t: &'static [Node]) -> Self {
        self.then = t;
        self
    }
    const fn pipes(mut self, p: Pipes) -> Self {
        self.pipes = p;
        self
    }
    const fn top_only(mut self) -> Self {
        self.top_only = true;
        self
    }
    const fn once(mut self) -> Self {
        self.once = true;
        self
    }
    /// The word as `?` lists it.
    pub fn shown(&self) -> String {
        match self.word {
            Word::Fixed(w) => w.to_string(),
            Word::Value(v) => v.placeholder().to_string(),
            Word::Path => "<path>".into(),
            Word::Terms => "<word>".into(),
            Word::Command(_) => "<command>".into(),
        }
    }
}

/// What a walk read: the fixed words, the values (each with its kind), the path or words, the
/// command after `run`/`top`, and the pipes before the filters.
#[derive(Debug, Default, Clone)]
pub struct Read {
    pub fixed: Vec<&'static str>,
    pub values: Vec<(Value, String)>,
    pub path: Vec<String>,
    pub terms: Vec<String>,
    pub sub: Option<Op>,
    pub compare: bool,
    pub display: Option<Display>,
    /// `| no-more`: all the output at once, never a screenful at a time.
    pub no_more: bool,
}

impl Read {
    fn has(&self, w: &str) -> bool {
        self.fixed.contains(&w)
    }
    fn value(&self, v: Value) -> Option<&str> {
        self.values.iter().find(|(k, _)| *k == v).map(|(_, s)| s.as_str())
    }
    fn number(&self, v: Value) -> Option<usize> {
        self.value(v).and_then(|n| n.parse().ok())
    }
}

// ── The commands ─────────────────────────────────────────────────────────────────────────────────

const CONFIGURATION_VIEWS: &[Display] = &[Display::Detail, Display::Set];
const DETAIL: &[Display] = &[Display::Detail];
const COMPARISON: &[Display] = &[Display::Diff];

static ROOTS: &[Node] = &[
    fixed("clear", "Clear something the machine holds", In::Op).then(&[fixed("system", "This machine", In::Op).then(&[fixed("commit", "Cancel the commit scheduled with commit at (it asks first)", In::Op).whole(|_| Ok(Op::ClearCommit))])]),
    fixed("commit", "Make the candidate the machine's configuration", In::Cf).whole(commit).pipes(Pipes::Steps).then(COMMIT_FIRST),
    fixed("configure", "Edit the configuration (a candidate, until you commit it)", In::Op).top_only().whole(|_| Ok(Op::Configure { exclusive: false })).then(&[fixed("exclusive", "With the candidate yours alone: your other sessions cannot change it until you leave", In::Op).whole(|_| Ok(Op::Configure { exclusive: true }))]),
    fixed("delete", "Remove a value, a setting, or everything below a place", In::Cf).whole(|r| Ok(Op::Delete { words: r.path.clone() })).then(&[node(Word::Path, "What to remove", In::Cf).whole(|r| Ok(Op::Delete { words: r.path.clone() }))]),
    fixed("edit", "Go down to a part of the configuration", In::Cf).then(&[node(Word::Path, "Where to go", In::Cf).whole(|r| Ok(Op::Edit { words: r.path.clone() }))]),
    fixed("exit", "Leave torqcli", In::Op).top_only().whole(|_| Ok(Op::Exit)),
    fixed("exit", "Up one level (out of configuration mode at the top; the candidate is kept)", In::Cf).top_only().whole(|_| Ok(Op::Exit)).then(&[fixed("configuration-mode", "Leave configuration mode from anywhere", In::Cf).whole(|_| Ok(Op::End))]),
    fixed("file", "The configuration's Nix files (list, show; edit and delete in configuration mode)", In::Both).then(FILE),
    fixed("help", "How to use torqcli; help <command> explains one, help <word> searches commands and settings", In::Both).whole(help).pipes(Pipes::Filters).then(&[node(Word::Terms, "A command, or words to search for", In::Both).whole(help).pipes(Pipes::Filters)]),
    fixed("load", "Load a saved or pasted configuration into the candidate (override, merge, set)", In::Cf).then(LOAD),
    fixed("quit", "The same as exit", In::Both).top_only().whole(|_| Ok(Op::Exit)),
    fixed("request", "Ask the machine to do something", In::Op).then(&[fixed("system", "This machine", In::Op).then(REQUEST_SYSTEM)]),
    fixed("rollback", "Load an earlier configuration into the candidate, or put back one part of it", In::Cf).whole(rollback).then(&[value(Value::Rollback, "0 is the configuration in effect, 1 the one before, …").whole(rollback).then(&[node(Word::Path, "Only this part: put back as that configuration has it", In::Cf).whole(rollback)])]),
    fixed("run", "Run an operational-mode command", In::Cf).then(&[node(Word::Command(Mode::Operational), "An operational command", In::Cf).whole(|r| Ok(Op::Run(Box::new(sub(r)?))))]),
    fixed("save", "Save the whole candidate, and its config files, to a file", In::Cf).then(&[value(Value::File, "Where to save it (written private: 0600)").whole(|r| Ok(Op::Save { file: r.value(Value::File).unwrap_or_default().to_string() }))]),
    fixed("set", "Set a value", In::Cf).then(&[node(Word::Path, "The setting and its value", In::Cf).whole(|r| Ok(Op::Set { words: r.path.clone() }))]),
    fixed("show", "Show information about the machine", In::Op).then(SHOW),
    fixed("show", "Show the candidate configuration, or what it changes (show | compare)", In::Cf).whole(show_candidate).pipes(Pipes::Candidate).then(&[node(Word::Path, "A part of it", In::Cf).whole(show_candidate).pipes(Pipes::Candidate)]),
    fixed("top", "Back to the top, or run a command from there", In::Cf).whole(|_| Ok(Op::Top { then: None })).then(&[node(Word::Command(Mode::Configuration), "A command, as from the top", In::Cf).whole(|r| Ok(Op::Top { then: Some(Box::new(sub(r)?)) }))]),
    fixed("up", "Up one level, or <n> levels", In::Cf).top_only().whole(up).then(&[value(Value::Levels, "How many levels up").whole(up)]),
];

static SHOW: &[Node] = &[
    fixed("configuration", "The configuration in effect", In::Op).whole(show_active).pipes(Pipes::Views(CONFIGURATION_VIEWS)).then(&[node(Word::Path, "A part of it", In::Op).whole(show_active).pipes(Pipes::Views(CONFIGURATION_VIEWS))]),
    fixed("interfaces", "The network interfaces, their state and addresses", In::Op).whole(interfaces).pipes(Pipes::Filters).then(&[value(Value::Interface, "One of them").whole(interfaces).pipes(Pipes::Filters)]),
    fixed("packages", "Search the software you can install", In::Op).then(&[node(Word::Terms, "Words to search for, in names and descriptions", In::Op).whole(|r| Ok(Op::ShowPackages { terms: r.terms.clone() })).pipes(Pipes::Filters)]),
    fixed("services", "The services the configuration turns on", In::Op).whole(|_| Ok(Op::ShowServices)).pipes(Pipes::Filters),
    fixed("system", "This machine", In::Op).then(SHOW_SYSTEM),
    fixed("users", "The people with an account here: where they are logged in, and their groups", In::Op).whole(|_| Ok(Op::ShowUsers)).pipes(Pipes::Filters),
    fixed("version", "What this machine runs: TorqOS, NixOS, Linux", In::Op).whole(|_| Ok(Op::ShowVersion)).pipes(Pipes::Filters),
];

static SHOW_SYSTEM: &[Node] = &[
    fixed("alarms", "Whether this machine is as its history says", In::Op).whole(|r| Ok(Op::ShowAlarms { display: r.display })).pipes(Pipes::Views(DETAIL)),
    fixed("backups", "The backups the configuration declares: whether each has its key, its snapshots, its last run", In::Op).whole(|_| Ok(Op::ShowBackups)).pipes(Pipes::Filters),
    fixed("commit", "The configurations so far, newest first", In::Op).whole(|r| Ok(Op::ShowCommits { display: r.display })).pipes(Pipes::Views(DETAIL)),
    fixed("desktop", "Your desktop: what of COSMIC's settings is saved in your configuration, and what is not yet", In::Op).whole(|_| Ok(Op::ShowDesktop)).pipes(Pipes::Filters),
    fixed("ledger", "Every change, as the ledger has it (refused and failed ones too)", In::Op).whole(ledger).pipes(Pipes::Filters).then(&[value(Value::Count, "How many entries (20)").whole(ledger).pipes(Pipes::Filters)]),
    fixed("proposals", "Changes proposed and waiting for a person (the agent's): request system apply <n> applies one", In::Op).whole(|_| Ok(Op::ShowProposals)).pipes(Pipes::Filters),
    fixed("remote", "The configuration's remote: where it and its ledger are pushed, and how the last push went", In::Op).whole(|_| Ok(Op::ShowRemote)).pipes(Pipes::Filters),
    fixed("rollback", "An earlier configuration, or how two differ", In::Op).then(&[value(Value::Rollback, "0 is the configuration in effect, 1 the one before, …").whole(show_rollback).pipes(Pipes::Views(CONFIGURATION_VIEWS)).then(&[fixed("compare", "How it differs from another one", In::Op).then(&[value(Value::Rollback, "The configuration to compare with").whole(show_rollback).pipes(Pipes::Views(COMPARISON))])])]),
    fixed("secrets", "The secrets: whose, whether each has a value, and what each may be used for (never a value)", In::Op).whole(|_| Ok(Op::ShowSecrets)).pipes(Pipes::Filters),
    fixed("snapshots", "The snapshots of the data this machine still has, each from before a change", In::Op).whole(|_| Ok(Op::ShowSnapshots)).pipes(Pipes::Filters),
    fixed("storage", "The disks: size and use, and the system versions kept", In::Op).whole(|_| Ok(Op::ShowStorage)).pipes(Pipes::Filters),
    fixed("uptime", "How long the machine has run, its load, and its last change", In::Op).whole(|_| Ok(Op::ShowUptime)).pipes(Pipes::Filters),
];

/// `… user <user>`: a person's own secret, not the system's.
/// After `set <name>`: its value from a file, and whose.
static SECRET_SET: &[Node] = &[
    fixed("file", "From a file (an SSH key, ssh-keygen's: several lines, not typed); read as you, not kept anywhere else", In::Op).then(&[value(Value::File, "The file (relative: to where torq runs)").whole(secret).then(SECRET_OWNER)]),
    fixed("user", "A person's own secret, not the system's: yours (no one else's, an administrator included, but to delete)", In::Op).then(&[value(Value::User, "Whose: yours").whole(secret)]),
];

static SECRET_OWNER: &[Node] = &[fixed("user", "A person's own secret, not the system's: yours (no one else's, an administrator included, but to delete)", In::Op).then(&[value(Value::User, "Whose: yours").whole(secret)])];

/// What a secret may be used for, or by whom: given (`allow`) or taken back (`disallow`).
static SECRET_GRANT: &[Node] = &[
    fixed("agent", "The agent (torq-agent): it uses the secret through TorqOS, never reading it", In::Op).whole(secret).then(SECRET_OWNER),
    fixed("https", "Sent in an HTTPS header to this host, and to no other (a token)", In::Op).then(&[value(Value::Host, "The host: a DNS name (api.github.com)").whole(secret).then(SECRET_OWNER)]),
    fixed("ssh", "Signs for SSH to this host, and for no other (an SSH key)", In::Op).then(&[value(Value::Host, "The host: a DNS name (github.com)").whole(secret).then(SECRET_OWNER)]),
];

static REQUEST_SECRET: &[Node] = &[
    fixed("allow", "Let it be used: for a host, or by the agent (one change, undoable)", In::Op).then(&[value(Value::Secret, "The secret's name").then(SECRET_GRANT)]),
    fixed("approve", "When a person is asked before the agent uses it (a tainted session is always asked)", In::Op).then(&[value(Value::Secret, "The secret's name").then(&[
        fixed("each-use", "Every time", In::Op).whole(secret).then(SECRET_OWNER),
        fixed("first-use", "The first time in each of the agent's sessions", In::Op).whole(secret).then(SECRET_OWNER),
        fixed("never", "Never: only a tainted session asks", In::Op).whole(secret).then(SECRET_OWNER),
    ])]),
    fixed("delete", "Delete its value and its rule (no undoing the value)", In::Op).then(&[value(Value::Secret, "The secret's name").whole(secret).then(SECRET_OWNER)]),
    fixed("disallow", "Take back a use: a host, or the agent's (one change, undoable)", In::Op).then(&[value(Value::Secret, "The secret's name").then(SECRET_GRANT)]),
    fixed("set", "Give its value, typed here (or from a file): kept encrypted, root's alone, outside the configuration, never in the ledger", In::Op).then(&[value(Value::Secret, "Its name: letters, digits, - and _").whole(secret).then(SECRET_SET)]),
    fixed("show", "Print its value, here (it asks your password)", In::Op).then(&[value(Value::Secret, "The secret's name").whole(secret).then(SECRET_OWNER)]),
];

static REQUEST_SYSTEM: &[Node] = &[
    fixed("agent", "The agent (torq-agent): what it may do without asking", In::Op).then(&[fixed("trust", "For <minutes> (at most 60), the agent's untainted proposals in your own folder apply without asking — never the system's; ledgered; it asks your password", In::Op).then(&[fixed("revoke", "End your trust in the agent now", In::Op).whole(|_| Ok(Op::Request(Request::TrustRevoke))), value(Value::Minutes, "1 to 60").whole(|r| Ok(Op::Request(Request::Trust { minutes: r.number(Value::Minutes).unwrap_or(0) as u32 })))])]),
    fixed("apply", "Apply a change someone else proposed (the agent's): it shows it and asks first", In::Op).then(&[value(Value::Change, "The proposal's number, from show system proposals").whole(|r| Ok(Op::Request(Request::Apply { proposal: r.value(Value::Change).map(|c| c.trim_start_matches('#')).unwrap_or_default().parse()? })))]),
    fixed("backup", "A backup the configuration declares (services restic backups)", In::Op).then(&[fixed("key", "Give its key, typed here: kept root's alone, outside the configuration, never in the ledger (it asks your password)", In::Op).then(&[value(Value::Backup, "The backup's name, from show system backups").whole(|r| Ok(Op::Request(Request::BackupKey { name: r.value(Value::Backup).unwrap_or_default().into() })))])]),
    fixed("desktop", "Your desktop (COSMIC's settings), kept in your own configuration", In::Op).then(&[
        fixed("save", "Save it now: what you changed goes into your configuration (no password; one change, undoable)", In::Op).whole(|_| Ok(Op::Request(Request::DesktopSave))),
        fixed("saving", "When it is saved", In::Op).then(&[
            fixed("automatic", "By itself, as you change it", In::Op).whole(|_| Ok(Op::Request(Request::DesktopSaving { automatic: true }))),
            fixed("manual", "Only when you ask (request system desktop save)", In::Op).whole(|_| Ok(Op::Request(Request::DesktopSaving { automatic: false }))),
        ]),
    ]),
    fixed("ledger", "The ledger", In::Op).then(&[fixed("acknowledge", "You have looked into where it and the repository disagree: say no more of it (ledgered; a new one is said as ever)", In::Op).whole(|_| Ok(Op::Request(Request::LedgerAcknowledge)))]),
    fixed("password", "Change a person's password (yours, or <user>'s)", In::Op).whole(password).then(&[value(Value::User, "Whose (yours, if none)").whole(password)]),
    fixed("pull", "Load the configuration waiting on the remote (its incoming branch) into the candidate: nothing changes until you commit it", In::Op).whole(|_| Ok(Op::Request(Request::Pull))),
    fixed("remote", "The configuration's remote", In::Op).then(&[fixed("clear", "Clear its alarm, having looked into it (ledgered; raised again if still true)", In::Op).whole(|_| Ok(Op::Request(Request::RemoteClear))), fixed("key", "This machine's key for it, to add to the repository as a deploy key (made the first time)", In::Op).whole(|_| Ok(Op::Request(Request::RemoteKey)))]),
    fixed("repair", "Return a machine changed outside TorqOS to its last configuration (it asks first)", In::Op).whole(|_| Ok(Op::Request(Request::Repair))),
    fixed("restore", "Put a path back from a backup, as its owner: what is there now is set aside, not lost (it asks your password)", In::Op).then(&[value(Value::Backup, "The backup's name, from show system backups").then(&[value(Value::Snapshot, "A snapshot's id, from show system backups — or, for the newest, the path").whole(restore).then(&[value(Value::File, "The path, under /home or /var/lib").whole(restore)])])]),
    fixed("revert", "Undo one change: a new change that reverses it (it asks first, unless it only touches your own settings)", In::Op).then(&[value(Value::Change, "The change's number, from show system ledger").whole(|r| Ok(Op::Request(Request::Revert { change: r.value(Value::Change).map(|c| c.trim_start_matches('#')).unwrap_or_default().parse()? })))]),
    fixed("secret", "A secret: a token or an SSH key, kept encrypted, that the agent may use and never read", In::Op).then(REQUEST_SECRET),
    fixed("snapshot", "The data as it was before a change", In::Op).then(&[fixed("delete", "Delete one, for the room or for what is in it: on the record first, and there is no undoing it (it asks your password)", In::Op).then(&[value(Value::Change, "The change's number, from show system snapshots").whole(|r| Ok(Op::Request(Request::SnapshotDelete { change: r.value(Value::Change).map(|c| c.trim_start_matches('#')).unwrap_or_default().parse()? })))]), fixed("restore", "Put a path back, as its owner: what is there now is set aside, not lost (it asks your password)", In::Op).then(&[value(Value::Change, "The change's number, from show system snapshots").then(&[value(Value::File, "The path, under /home or /var/lib").whole(|r| Ok(Op::Request(Request::RestoreData { change: r.value(Value::Change).map(|c| c.trim_start_matches('#')).unwrap_or_default().parse()?, path: r.value(Value::File).map(String::from).unwrap_or_default() })))])]), fixed("take", "Take one now, before work you may think better of (it asks your password)", In::Op).whole(|_| Ok(Op::Request(Request::SnapshotTake)))]),
    fixed("software", "This machine's software", In::Op).then(&[fixed("rollback", "Load an earlier configuration's software into the candidate (a downgrade)", In::Op).then(&[value(Value::Rollback, "0 is the configuration in effect, 1 the one before, …").whole(|r| Ok(Op::Request(Request::SoftwareRollback { n: r.number(Value::Rollback).unwrap_or(0), detail: r.display.is_some() }))).pipes(Pipes::Steps)])]),
    fixed("update", "Load the newest TorqOS into the candidate (nothing changes until you commit it)", In::Op).whole(update).pipes(Pipes::Steps).then(&[fixed("all", "Every input, nixpkgs too", In::Op).whole(update).pipes(Pipes::Steps)]),
];

static FILE: &[Node] = &[
    fixed("delete", "Delete one, in the candidate", In::Cf).then(&[value(Value::NixFile, "A .nix file of the configuration (file list)").whole(|r| file(r, FileAction::Delete))]),
    fixed("edit", "Edit one (or a new one) in your editor, in the candidate", In::Cf).then(&[value(Value::NixFile, "A .nix file of the configuration (file list)").whole(|r| file(r, FileAction::Edit))]),
    fixed("list", "Every Nix file of the configuration", In::Both).whole(|r| file(r, FileAction::List)).pipes(Pipes::Filters),
    fixed("show", "Print one", In::Both).then(&[value(Value::NixFile, "A .nix file of the configuration (file list)").whole(|r| file(r, FileAction::Show)).pipes(Pipes::Filters)]),
];

static LOAD: &[Node] = &[
    fixed("merge", "Set the file's settings onto the candidate", In::Cf).then(LOAD_FROM),
    fixed("override", "The file becomes the candidate", In::Cf).then(LOAD_FROM),
    fixed("set", "Run the file's `set` and `delete` lines (what `| display set` writes)", In::Cf).then(LOAD_FROM),
];

static LOAD_FROM: &[Node] = &[
    fixed("terminal", "What you paste or type next, ended by Ctrl-D at a new line", In::Cf).whole(load),
    value(Value::File, "A configuration saved with `save` (or `| display set` lines)").whole(load),
];

/// Right after `commit`: `check` stands alone; the rest combine, each once.
static COMMIT_FIRST: &[Node] = &[
    fixed("and-quit", "Commit, then leave configuration mode", In::Cf).once().whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE),
    fixed("at", "Commit at a time: approved now, applied then (clear system commit cancels it)", In::Cf).once().then(&AT),
    fixed("check", "Check the candidate evaluates, without committing", In::Cf).once().whole(commit).pipes(Pipes::Steps),
    fixed("comment", "Commit, with a comment for the log", In::Cf).once().then(&COMMENT),
    fixed("confirmed", "Commit, rolled back unless confirmed within minutes (default 10)", In::Cf).once().whole(commit).pipes(Pipes::Steps).then(&CONFIRMED),
];
static COMMIT_MORE: [Node; 4] = [
    fixed("and-quit", "Commit, then leave configuration mode", In::Cf).once().whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE),
    fixed("at", "Commit at a time: approved now, applied then (clear system commit cancels it)", In::Cf).once().then(&AT),
    fixed("comment", "Commit, with a comment for the log", In::Cf).once().then(&COMMENT),
    fixed("confirmed", "Commit, rolled back unless confirmed within minutes (default 10)", In::Cf).once().whole(commit).pipes(Pipes::Steps).then(&CONFIRMED),
];
static COMMENT: [Node; 1] = [value(Value::Text, "What the change is for").whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE)];
static AT: [Node; 1] = [value(Value::Time, "HH:MM (today, or tomorrow if it has passed), \"YYYY-MM-DD HH:MM\" (local), or RFC 3339; within 30 days").whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE)];
static CONFIRMED: [Node; 4] = [
    value(Value::Minutes, "1 to 1440: confirm by committing again").whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE),
    fixed("and-quit", "Commit, then leave configuration mode", In::Cf).once().whole(commit).pipes(Pipes::Steps).then(&COMMIT_MORE),
    fixed("at", "Commit at a time: approved now, applied then (clear system commit cancels it)", In::Cf).once().then(&AT),
    fixed("comment", "Commit, with a comment for the log", In::Cf).once().then(&COMMENT),
];

// ── Building an Op ───────────────────────────────────────────────────────────────────────────────

fn help(r: &Read) -> Result<Op> {
    Ok(Op::Help { topic: r.terms.clone() })
}

fn sub(r: &Read) -> Result<Op> {
    r.sub.clone().ok_or_else(|| anyhow::anyhow!("a command must follow"))
}

/// `d` if `what` offers it.
fn offered(what: &str, allowed: &[Display], d: Option<Display>) -> Result<Option<Display>> {
    match d {
        Some(d) if !allowed.contains(&d) => {
            let names: Vec<String> = allowed.iter().map(|a| format!("| display {}", a.name())).collect();
            bail!("| display {} is not for {what} (it has {})", d.name(), if names.is_empty() { "none".into() } else { names.join(", ") })
        }
        d => Ok(d),
    }
}

fn show_candidate(r: &Read) -> Result<Op> {
    if r.compare {
        if !r.path.is_empty() {
            bail!("| compare compares the whole candidate: `show | compare`");
        }
        return Ok(Op::Compare { with: r.number(Value::Rollback), display: offered("a comparison", COMPARISON, r.display)? });
    }
    Ok(Op::ShowCandidate { path: r.path.clone(), display: offered("a configuration", CONFIGURATION_VIEWS, r.display)? })
}

fn show_active(r: &Read) -> Result<Op> {
    Ok(Op::ShowActive { path: r.path.clone(), display: r.display })
}

fn show_rollback(r: &Read) -> Result<Op> {
    let n: Vec<usize> = r.values.iter().filter(|(k, _)| *k == Value::Rollback).filter_map(|(_, v)| v.parse().ok()).collect();
    Ok(Op::ShowRollback { n: n.first().copied().unwrap_or(0), compare: n.get(1).copied(), display: r.display })
}

/// `request system secret <act> <name> … [user <user>]`, from the words read.
fn secret(r: &Read) -> Result<Op> {
    use crate::secrets::{Act, Grant};
    use torq_wire::secrets::Approve;
    let host = || r.value(Value::Host).unwrap_or_default().to_string();
    let grant = || {
        if r.has("https") {
            Grant::Https(host())
        } else if r.has("ssh") {
            Grant::Ssh(host())
        } else {
            Grant::Agent
        }
    };
    let act = if r.has("allow") {
        Act::Allow(grant())
    } else if r.has("disallow") {
        Act::Disallow(grant())
    } else if r.has("approve") {
        Act::Approve(["each-use", "first-use", "never"].into_iter().find(|w| r.has(w)).and_then(Approve::from_word).unwrap_or_default())
    } else if r.has("set") && r.has("file") {
        Act::SetFile(r.value(Value::File).unwrap_or_default().to_string())
    } else if r.has("set") {
        Act::Set
    } else if r.has("show") {
        Act::Show
    } else {
        Act::Delete
    };
    Ok(Op::Request(Request::Secret { name: r.value(Value::Secret).unwrap_or_default().into(), user: r.value(Value::User).map(String::from), act }))
}

fn interfaces(r: &Read) -> Result<Op> {
    Ok(Op::ShowInterfaces { name: r.value(Value::Interface).map(String::from) })
}

fn ledger(r: &Read) -> Result<Op> {
    Ok(Op::ShowLedger { count: r.number(Value::Count) })
}

fn password(r: &Read) -> Result<Op> {
    Ok(Op::Request(Request::Password { user: r.value(Value::User).map(String::from) }))
}

/// `request system restore <backup> [<snapshot>] <path>`: one word after the name is the path (the
/// newest snapshot); two, the snapshot and the path.
fn restore(r: &Read) -> Result<Op> {
    let name = r.value(Value::Backup).unwrap_or_default().to_string();
    let first = r.value(Value::Snapshot).unwrap_or_default();
    Ok(Op::Request(match r.value(Value::File) {
        Some(_) if first.starts_with('/') => bail!("'{first}' is a path: the snapshot comes before it (request system restore {name} <snapshot> <path>)"),
        Some(path) => Request::Restore { name, snapshot: Some(first.to_string()), path: path.to_string() },
        None if first.starts_with('/') => Request::Restore { name, snapshot: None, path: first.to_string() },
        None => bail!("needs <file> (the path, under /home or /var/lib) after the snapshot"),
    }))
}

fn update(r: &Read) -> Result<Op> {
    Ok(Op::Request(Request::Update { all: r.has("all"), detail: r.display.is_some() }))
}

fn file(r: &Read, action: FileAction) -> Result<Op> {
    Ok(Op::File { action, path: r.value(Value::NixFile).map(String::from) })
}

fn load(r: &Read) -> Result<Op> {
    let how = if r.has("merge") {
        LoadHow::Merge
    } else if r.has("override") {
        LoadHow::Override
    } else {
        LoadHow::Set
    };
    let from = r.value(Value::File).map(|f| LoadFrom::File(f.to_string())).unwrap_or(LoadFrom::Terminal);
    Ok(Op::Load { how, from })
}

fn rollback(r: &Read) -> Result<Op> {
    Ok(Op::Rollback { n: r.number(Value::Rollback).unwrap_or(0), path: r.path.clone() })
}

fn up(r: &Read) -> Result<Op> {
    Ok(Op::Up { n: r.number(Value::Levels).unwrap_or(1) })
}

fn commit(r: &Read) -> Result<Op> {
    if r.has("at") && r.has("confirmed") {
        bail!("commit at and commit confirmed do not go together: a scheduled commit lands with no one there to confirm it");
    }
    Ok(Op::Commit(CommitOptions {
        check: r.has("check"),
        confirm: r.has("confirmed").then(|| r.value(Value::Minutes).and_then(|m| m.parse().ok()).unwrap_or(10)),
        comment: r.value(Value::Text).map(String::from),
        at: r.value(Value::Time).map(String::from),
        and_quit: r.has("and-quit"),
        detail: r.display.is_some(),
    }))
}

// ── The walk ─────────────────────────────────────────────────────────────────────────────────────

/// Where a walk stands: where a mode starts (`top`: not after `run` or `top`), or after a node.
#[derive(Clone, Copy)]
enum Here {
    Start { mode: Mode, top: bool },
    After(&'static Node, Mode),
}

impl Here {
    fn mode(self) -> Mode {
        match self {
            Here::Start { mode, .. } | Here::After(_, mode) => mode,
        }
    }
    /// What may stand next, as `read` stands.
    fn next(self, read: &Read) -> Vec<&'static Node> {
        let (list, mode, top): (&'static [Node], Mode, bool) = match self {
            Here::Start { mode, top } => (ROOTS, mode, top),
            Here::After(n, mode) => match n.word {
                // A path or words go on to the end.
                Word::Path | Word::Terms => return vec![n],
                _ => (n.then, mode, true),
            },
        };
        list.iter().filter(|n| n.mode.has(mode) && (top || !n.top_only) && !(n.once && matches!(n.word, Word::Fixed(w) if read.has(w)))).collect()
    }
    fn node(self) -> Option<&'static Node> {
        match self {
            Here::After(n, _) => Some(n),
            Here::Start { .. } => None,
        }
    }
}

/// Why a walk stopped.
#[derive(Debug)]
pub enum Stop {
    /// A word that may not stand where it is: why, with what may, or advice.
    Word { said: String },
    /// A value that is not one.
    Value { why: String },
    /// The line ends before it is a command: what may follow.
    Short { offered: Vec<String>, needs: Option<(String, String)> },
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stop::Word { said, .. } => write!(f, "{said}"),
            Stop::Value { why } => write!(f, "{why}"),
            Stop::Short { needs: Some((p, help)), offered, .. } if offered.is_empty() => write!(f, "needs {p} ({})", lower(help)),
            Stop::Short { needs: Some((p, _)), offered, .. } => write!(f, "not a whole command: {p} or one of: {}", offered.join(", ")),
            Stop::Short { offered, .. } => write!(f, "not a whole command: one of these may follow: {}", offered.join(", ")),
        }
    }
}

impl std::error::Error for Stop {}

/// A help sentence inside another sentence: its first letter small, no full stop.
fn lower(help: &str) -> String {
    let t = help.trim_end_matches('.');
    let mut c = t.chars();
    c.next().map(|f| f.to_lowercase().chain(c).collect()).unwrap_or_default()
}

/// A walk of a line's command words (up to any `|`): the words as they are meant (abbreviations
/// expanded), where it ended, what it read, and the commands it is inside (`run`, `top`: the
/// outermost first), each with what it read.
pub struct Walked {
    pub words: Vec<String>,
    here: Here,
    pub read: Read,
    outer: Vec<(&'static Node, Read)>,
}

impl Walked {
    /// The last node of the innermost command (its pipes, whether it is whole).
    pub fn node(&self) -> Option<&'static Node> {
        self.here.node()
    }
    /// What may follow, for `?`.
    /// What may follow: the words here, but never a command inside itself (`top top …`: once leads
    /// there already) — so a line nests no deeper than `top run …` (audit of 2026-11-01, L4).
    pub fn next(&self) -> Vec<&'static Node> {
        self.here.next(&self.read).into_iter().filter(|n| !self.reenters(n, None)).collect()
    }

    /// Whether `n` leads into a command the line is already inside (or into `entering`, the one it
    /// is about to enter).
    fn reenters(&self, n: &Node, entering: Option<&Node>) -> bool {
        n.then.iter().any(|c| matches!(c.word, Word::Command(_)) && (entering.is_some_and(|e| std::ptr::eq(e, c)) || self.outer.iter().any(|(o, _)| std::ptr::eq(*o, c))))
    }
    pub fn mode(&self) -> Mode {
        self.here.mode()
    }
    /// The line's `Op`: the innermost command's, inside each command around it.
    fn op(&self) -> Result<Op> {
        let node = self.node().filter(|n| n.whole.is_some()).ok_or_else(|| short(self))?;
        let mut op = (node.whole.expect("whole"))(&self.read)?;
        for (n, read) in self.outer.iter().rev() {
            let read = Read { sub: Some(op), ..read.clone() };
            op = (n.whole.expect("a command around another is whole"))(&read)?;
        }
        Ok(op)
    }
}

/// Walk `words` (command words only: no `|`) in `mode`: each word is the one that stands there, a
/// unique beginning of one (abbreviations), or a value, path or search word where one may stand.
pub fn walk(mode: Mode, words: &[String]) -> Result<Walked, Stop> {
    let mut walked = Walked { words: vec![], here: Here::Start { mode, top: true }, read: Read::default(), outer: vec![] };
    for w in words {
        step(&mut walked, w)?;
    }
    Ok(walked)
}

/// One word more.
fn step(walked: &mut Walked, w: &String) -> Result<(), Stop> {
    let here = walked.here;
    let next = walked.next();
    // A whole command of another mode (`run`, `top`): from here on, that command, from its start.
    if let Some(n) = next.iter().find(|n| matches!(n.word, Word::Command(_))) {
        let Word::Command(m) = n.word else { unreachable!() };
        walked.outer.push((n, std::mem::take(&mut walked.read)));
        walked.here = Here::Start { mode: m, top: false };
        return step(walked, w);
    }
    if let Some(n) = next.iter().find(|n| matches!(n.word, Word::Path | Word::Terms)) {
        if matches!(n.word, Word::Path) { walked.read.path.push(w.clone()) } else { walked.read.terms.push(w.clone()) }
        walked.words.push(w.clone());
        walked.here = Here::After(n, here.mode());
        return Ok(());
    }
    let name = |n: &Node| match n.word {
        Word::Fixed(f) => f,
        _ => "",
    };
    let fixed: Vec<&'static Node> = next.iter().copied().filter(|n| matches!(n.word, Word::Fixed(_))).collect();
    let valued = next.iter().find(|n| matches!(n.word, Word::Value(_)));
    let exact = fixed.iter().find(|n| name(n) == w);
    // A unique beginning of a word — only where no value may stand (`load merge term` is a file).
    let begins: Vec<&'static Node> = if exact.is_none() && valued.is_none() && !w.is_empty() { fixed.iter().copied().filter(|n| name(n).starts_with(w.as_str())).collect() } else { vec![] };
    if let Some(n) = exact.copied().or(if begins.len() == 1 { Some(begins[0]) } else { None }) {
        walked.read.fixed.push(name(n));
        walked.words.push(name(n).to_string());
        walked.here = Here::After(n, here.mode());
        return Ok(());
    }
    if let Some(n) = valued {
        let Word::Value(v) = n.word else { unreachable!() };
        v.check(w).map_err(|e| Stop::Value { why: format!("{e:#}") })?;
        walked.read.values.push((v, w.clone()));
        walked.words.push(w.clone());
        walked.here = Here::After(n, here.mode());
        return Ok(());
    }
    let offered: Vec<String> = fixed.iter().map(|n| name(n).to_string()).collect();
    let after = walked.words.clone();
    // The word of a command in the other mode, where it stands there.
    let other = if here.mode() == Mode::Operational { Mode::Configuration } else { Mode::Operational };
    let there = match here {
        Here::Start { top, .. } => Here::Start { mode: other, top },
        Here::After(n, _) => Here::After(n, other),
    };
    // (Only where a line starts: after `run`, the command is the operational one `run` takes.)
    let elsewhere = walked.outer.is_empty() && there.next(&walked.read).iter().any(|n| name(n) == w);
    let said = if begins.len() > 1 {
        let many: Vec<&str> = begins.iter().map(|n| name(n)).collect();
        format!("'{w}' is ambiguous{}: {}", after_text(&after), many.join(", "))
    } else if elsewhere && other == Mode::Configuration {
        format!("'{w}' is a configuration-mode command{}: `configure` first", after_text(&after))
    } else if elsewhere {
        format!("'{w}' is an operational-mode command{}: `run {}` runs it from here", after_text(&after), after.iter().map(String::as_str).chain([w.as_str(), "…"]).collect::<Vec<_>>().join(" "))
    } else if offered.is_empty() {
        format!("'{w}' is not a word here: nothing may follow `{}`", after.join(" "))
    } else {
        refused(w, &after, &offered)
    };
    Err(Stop::Word { said })
}

fn after_text(after: &[String]) -> String {
    if after.is_empty() { String::new() } else { format!(" (after `{}`)", after.join(" ")) }
}

/// Why `w` may not stand after `after`: advice for another CLI's word, a near word, or what may
/// stand there.
fn refused(w: &str, after: &[String], offered: &[String]) -> String {
    let at = after_text(after);
    let foreign = match after.last() {
        Some(x) if x == "|" => Some(crate::vocabulary::At::Pipe),
        None => Some(crate::vocabulary::At::Command),
        Some(_) => None,
    };
    if let Some(advice) = foreign.and_then(|at| crate::vocabulary::instead(at, w)) {
        return format!("'{w}' is not a word here{at}: {advice}");
    }
    let hint = crate::near::hint(w, offered.iter().map(String::as_str));
    format!("'{w}' is not a word here{at}{hint} (one of: {})", offered.join(", "))
}

/// Why a walked line is not a whole command.
pub fn short(walked: &Walked) -> Stop {
    let next = walked.next();
    let offered: Vec<String> = next.iter().filter_map(|n| match n.word {
        Word::Fixed(f) => Some(f.to_string()),
        _ => None,
    }).collect();
    let needs = next.iter().find(|n| !matches!(n.word, Word::Fixed(_))).map(|n| (n.shown(), n.help.to_string()));
    Stop::Short { offered, needs }
}

/// A word that stands where `offered` may: itself, or a unique beginning of one (as command words
/// abbreviate); otherwise why not.
fn resolve(w: &str, offered: &[String], after: &[String]) -> Result<String> {
    if offered.iter().any(|o| o == w) {
        return Ok(w.to_string());
    }
    let begins: Vec<&String> = offered.iter().filter(|o| !w.is_empty() && o.starts_with(w)).collect();
    match begins.as_slice() {
        [one] => Ok(one.to_string()),
        [] => bail!("{}", refused(w, after, offered)),
        many => bail!("'{w}' is ambiguous{}: {}", after_text(after), many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

/// A line's pipes after `node`'s command (`before`): its own (`| compare`, `| display <view>`), then
/// the filters, each word as it is meant (abbreviations expanded). Reads them into `read`; the
/// filters are returned.
fn apply_pipes(node: &Node, segments: &mut [Vec<String>], read: &mut Read, before: &[String]) -> Result<Vec<crate::filter::Filter>> {
    let filter_names: Vec<String> = crate::filter::FILTERS.iter().map(|t| t.name.to_string()).collect();
    let mut filters = Vec::new();
    let mut after: Vec<String> = before.to_vec();
    for seg in segments.iter_mut() {
        after.push("|".into());
        if node.pipes == Pipes::None {
            bail!("`{}` prints nothing to filter", before.join(" "));
        }
        let own = if filters.is_empty() { pipe_words(node, read) } else { vec![] };
        // A command's steps are no output to filter: `| display detail` alone.
        let filter_names: &[String] = if node.pipes == Pipes::Steps { &[] } else { &filter_names };
        let paging = (!read.no_more && node.pipes != Pipes::Steps).then(|| "no-more".to_string());
        let offered: Vec<String> = own.iter().chain(filter_names).chain(&paging).cloned().collect();
        let Some(first) = seg.first().cloned() else { bail!("a | needs something after it (one of: {})", offered.join(", ")) };
        let word = match resolve(&first, &offered, &after) {
            Ok(w) => w,
            Err(e) if !filters.is_empty() && pipe_words(node, read).iter().any(|p| p.starts_with(first.as_str())) => {
                bail!("| {first} must come before the filters: they work on the output, last ({e:#})")
            }
            Err(e) => return Err(e),
        };
        seg[0] = word.clone();
        after.push(word.clone());
        match word.as_str() {
            "display" if seg.len() > 2 => bail!("'{}' is not a word here: nothing may follow `{}`", seg[2], after.join(" ")),
            // `| compare rollback <n>`: against configuration n (read as the line's Rollback value).
            "compare" => {
                if let Some(w) = seg.get(1).cloned() {
                    seg[1] = resolve(&w, &["rollback".to_string()], &after)?;
                    let at = format!("{} rollback", after.join(" "));
                    let Some(n) = seg.get(2) else { bail!("| compare rollback needs <number> (0 is the configuration in effect, 1 the one before, …)") };
                    Value::Rollback.check(n)?;
                    if let Some(extra) = seg.get(3) {
                        bail!("'{extra}' is not a word here: nothing may follow `{at} {n}`");
                    }
                    read.values.push((Value::Rollback, n.clone()));
                }
                read.compare = true;
            }
            "no-more" if seg.len() > 1 => bail!("'{}' is not a word here: nothing may follow `| no-more`", seg[1]),
            "no-more" => read.no_more = true,
            "display" => {
                let views = views(node, read);
                let Some(v) = seg.get(1).cloned() else { bail!("needs <view> (one of: {})", views.join(", ")) };
                let view = resolve(&v, &views, &after).map_err(|_| anyhow::anyhow!("'{v}' is not a view (after `| display`) (one of: {})", views.join(", ")))?;
                seg[1] = view.clone();
                read.display = Some(match view.as_str() {
                    "set" => Display::Set,
                    "detail" => Display::Detail,
                    _ => Display::Diff,
                });
            }
            _ => {
                if let Some((what, like)) = crate::filter::tool(&word).and_then(|t| t.needs).filter(|_| seg.len() == 1) {
                    bail!("| {word} needs {what}, as in `{like}`");
                }
                filters.push(crate::filter::Filter { tool: word, args: seg[1..].to_vec() })
            }
        }
        after.extend(seg[1..].iter().cloned());
    }
    if filters.len() > MAX_FILTERS {
        bail!("at most {MAX_FILTERS} filters on one line");
    }
    Ok(filters)
}

/// The pipe words a command takes next (`compare`, `display`), as `read` stands.
pub fn pipe_words(node: &Node, read: &Read) -> Vec<String> {
    let mut w = Vec::new();
    if node.pipes == Pipes::Candidate && !read.compare && read.display.is_none() {
        w.push("compare".to_string());
    }
    if matches!(node.pipes, Pipes::Candidate | Pipes::Views(_) | Pipes::Steps) && read.display.is_none() && !views(node, read).is_empty() {
        w.push("display".into());
    }
    w
}

/// The views `| display` offers, as `read` stands.
pub fn views(node: &Node, read: &Read) -> Vec<String> {
    let v: &[Display] = match node.pipes {
        Pipes::Candidate if read.compare => COMPARISON,
        Pipes::Candidate => CONFIGURATION_VIEWS,
        Pipes::Views(v) => v,
        Pipes::Steps => DETAIL,
        _ => &[],
    };
    v.iter().map(|d| d.name().to_string()).collect()
}

/// A whole line, read: its `Op`, its filters, and whether its output may be shown a screenful at a
/// time (not after `| no-more`).
pub struct Line {
    pub op: Op,
    pub filters: Vec<crate::filter::Filter>,
    pub page: bool,
}

/// Read a whole line.
pub fn read_line(mode: Mode, w: &[String]) -> Result<Line> {
    let bar = w.iter().position(|x| x == "|").unwrap_or(w.len());
    let mut walked = walk(mode, &w[..bar])?;
    let Some(node) = walked.node().filter(|n| n.whole.is_some()) else { return Err(short(&walked).into()) };
    let mut segments: Vec<Vec<String>> = if bar < w.len() { w[bar + 1..].split(|x| x == "|").map(<[String]>::to_vec).collect() } else { vec![] };
    let filters = apply_pipes(node, &mut segments, &mut walked.read, &walked.words)?;
    let op = walked.op()?;
    Ok(Line { op, filters, page: !walked.read.no_more })
}

/// The most filters one line may chain (each runs in its own sandbox).
const MAX_FILTERS: usize = 16;

/// One thing that may come next, for `?` and Tab.
pub enum Offer {
    /// A word, and its help.
    Word(&'static str, &'static str),
    /// A value: its placeholder, and help, and what kind (where one is known: its values offered).
    Value(String, &'static str, Option<Value>),
    /// A configuration path, for the tree to offer: the command it is for (`set`, `delete`, `edit`,
    /// `show`), and whether it is from the top (operational mode, `top`) or from where `edit` is.
    Path { verb: &'static str, from_top: bool },
}

/// What may come next after a walk's words.
pub fn offers(walked: &Walked) -> Vec<Offer> {
    let from_top = walked.mode() == Mode::Operational || walked.outer.iter().any(|(_, r)| r.has("top"));
    let mut out = Vec::new();
    for n in walked.next() {
        match n.word {
            Word::Fixed(w) => out.push(Offer::Word(w, n.help)),
            Word::Value(v) => out.push(Offer::Value(n.shown(), n.help, Some(v))),
            Word::Terms => out.push(Offer::Value(n.shown(), n.help, None)),
            Word::Path => out.push(Offer::Path { verb: walked.read.fixed.first().copied().unwrap_or("show"), from_top }),
            Word::Command(m) => {
                let start = Here::Start { mode: m, top: false };
                out.extend(start.next(&Read::default()).into_iter().filter(|x| !walked.reenters(x, Some(n))).filter_map(|x| match x.word {
                    Word::Fixed(w) => Some(Offer::Word(w, x.help)),
                    _ => None,
                }));
            }
        }
    }
    out
}

/// What `|` after a line offers, as its help says it — the line's own pipes and the filters it may
/// still take (`show version`: cut, grep, …; `show configuration`: display, cut, …); None where the
/// line's output may not be piped.
pub fn pipe_help(mode: Mode, words: &[String]) -> Option<String> {
    let line: Vec<String> = words.iter().cloned().chain(std::iter::once("|".to_string())).collect();
    let next = pipe_next(mode, &line);
    let first: Vec<&str> = next.iter().take(3).map(|(w, _, _)| w.as_str()).collect();
    (!first.is_empty()).then(|| format!("Pipe the output ({}, …)", first.join(", ")))
}

/// What may come after the last `|` of a line, its help, and the kind of value a placeholder is: the
/// command's own pipes, then the filters; after `| display`, the views; after `| compare rollback`,
/// a configuration; after a filter's name, what it needs (its own options: not offered).
pub fn pipe_next(mode: Mode, words: &[String]) -> Vec<(String, String, Option<Value>)> {
    let Some(bar) = words.iter().position(|x| x == "|") else { return vec![] };
    let Ok(mut walked) = walk(mode, &words[..bar]) else { return vec![] };
    let Some(node) = walked.node().filter(|n| n.whole.is_some() && n.pipes != Pipes::None) else { return vec![] };
    let mut segments: Vec<Vec<String>> = words[bar + 1..].split(|x| x == "|").map(<[String]>::to_vec).collect();
    let last = segments.pop().unwrap_or_default();
    let Ok(filters) = apply_pipes(node, &mut segments, &mut walked.read, &walked.words) else { return vec![] };
    match last.as_slice() {
        [] => {
            let mut out: Vec<(String, String, Option<Value>)> = Vec::new();
            if filters.is_empty() {
                for w in pipe_words(node, &walked.read) {
                    let help = match (w.as_str(), node.pipes) {
                        ("compare", _) => "What the candidate changes",
                        (_, Pipes::Steps) => "Show what it does as it does it",
                        _ => "Show it differently",
                    };
                    out.push((w, help.to_string(), None));
                }
            }
            if node.pipes == Pipes::Steps {
                return out;
            }
            out.extend(crate::filter::FILTERS.iter().map(|t| (t.name.to_string(), t.help.to_string(), None)));
            if !walked.read.no_more {
                out.push(("no-more".into(), "All of it at once, not a screenful at a time".into(), None));
            }
            out
        }
        [c] if c == "compare" && filters.is_empty() => vec![("rollback".into(), "What it changes from an earlier configuration".into(), None)],
        [c, r] if c == "compare" && r == "rollback" && filters.is_empty() => vec![(Value::Rollback.placeholder().into(), "0 is the configuration in effect, 1 the one before, …".into(), Some(Value::Rollback))],
        [d] if d == "display" && filters.is_empty() => views(node, &walked.read).into_iter().map(|v| {
            let help = match v.as_str() {
                "set" => "As set commands",
                "detail" if node.pipes == Pipes::Candidate || matches!(node.pipes, Pipes::Views(x) if x.contains(&Display::Set)) => "With the files and commit behind it",
                "detail" if node.pipes == Pipes::Steps => "Each step as it happens, how long it took, and Nix's and the broker's own output",
                "detail" => "With the commits and store paths behind it",
                _ => "As the raw diff of the files (what the broker records)",
            };
            (v, help.to_string(), None)
        }).collect(),
        // A filter that needs something: what (its own options are its own: not offered).
        [f] => crate::filter::tool(f).and_then(|t| t.needs).map(|(what, like)| vec![(what.to_string(), format!("As in `{like}`"), None)]).unwrap_or_default(),
        _ => vec![],
    }
}

/// Every command of a mode as it is typed (values as their placeholders), with its help: what
/// `help` lists and searches. Options that combine (commit's) are listed once each, right after
/// their command, not in every order.
pub fn commands(mode: Mode) -> Vec<(String, &'static str)> {
    fn go(list: &'static [Node], mode: Mode, top: bool, at: &str, in_options: bool, out: &mut Vec<(String, &'static str)>) {
        for n in list.iter().filter(|n| n.mode.has(mode) && (top || !n.top_only)) {
            if in_options && n.once {
                continue;
            }
            let line = if at.is_empty() { n.shown() } else { format!("{at} {}", n.shown()) };
            if n.whole.is_some() || matches!(n.word, Word::Path | Word::Terms) {
                out.push((line.clone(), n.help));
            }
            if !matches!(n.word, Word::Command(_)) {
                go(n.then, mode, true, &line, in_options || n.once, out);
            }
        }
    }
    let mut out = Vec::new();
    go(ROOTS, mode, true, "", false, &mut out);
    out
}

/// The first words of a mode's commands, with their help (what `help` lists).
pub fn first_words(mode: Mode) -> Vec<(&'static str, &'static str)> {
    Here::Start { mode, top: true }.next(&Read::default()).into_iter().filter_map(|n| match n.word {
        Word::Fixed(w) => Some((w, n.help)),
        _ => None,
    }).collect()
}

/// The help for the command `words` begin with, if they name one: its help, and what may follow
/// it (each with its help).
pub fn describe(mode: Mode, words: &[String]) -> Option<(String, &'static str, Vec<(String, &'static str)>)> {
    let walked = walk(mode, words).ok()?;
    let node = walked.node()?;
    if !matches!(node.word, Word::Fixed(_)) || walked.read.terms.len() + walked.read.path.len() + walked.read.values.len() > 0 {
        return None;
    }
    let next = offers(&walked)
        .into_iter()
        .filter_map(|o| match o {
            Offer::Word(w, h) => Some((w.to_string(), h)),
            Offer::Value(w, h, _) => Some((w, h)),
            Offer::Path { .. } => Some(("<path>".to_string(), "A place in the configuration (`?` lists them)")),
        })
        .collect();
    Some((walked.words.join(" "), node.help, next))
}
