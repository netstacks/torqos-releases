//! What TorqOS Settings (ADR-045 2) asks of the command line: the same calls to the broker and the same
//! words, so the window says what `torqcli` would print. Nothing here is the window's own: a word the GUI
//! has and the command line has not is a bug in one of them.

use crate::{history, status, Cli, BROKER, REPO};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// The machine, as torqcli sees it: its broker's socket and its configuration repository.
fn cli() -> Cli {
    Cli { repo: PathBuf::from(REPO), broker: BROKER.into() }
}

/// TorqOS's version as the machine runs it now: a panel started before an update is told the new one.
fn version() -> String {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    release.lines().find_map(|l| l.strip_prefix("VERSION_ID=")).map(|v| v.trim_matches('"').to_string()).filter(|v| !v.is_empty()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

/// The machine at a glance: `show system alarms`.
#[derive(Debug, Clone)]
pub struct Machine {
    /// TorqOS's version.
    pub version: String,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// What `show system alarms` prints, each line a sentence.
    pub alarms: String,
}

pub fn machine() -> Result<Machine> {
    let (_, report) = crate::status_report(&cli(), None, crate::run::watch_log()?)?;
    Ok(Machine { version: version(), well: matches!(report.fix, status::Fix::Nothing) && report.exit == 0, alarms: report.text })
}

/// An error as the window shows it: cleaned, as the terminal cleans what it prints.
pub fn said(e: &anyhow::Error) -> String {
    history::clean(&format!("{e:#}"))
}

/// The last `limit` changes, oldest first: `show system ledger`'s lines.
pub fn history(limit: usize) -> Result<Vec<history::Row>> {
    let entries = crate::read_ledger(&cli())?;
    Ok(history::rows(&entries, limit, &history::Clock::system()))
}

/// What the running system needs before it is all in use (ADR-049). A restart is almost never one of them: the
/// machine goes on working on the kernel it started with, and a boot image is only read at boot.
#[derive(Debug, Clone, PartialEq)]
pub enum Needs {
    Nothing,
    /// In use at the next start, with nothing to do until then: said, never asked for.
    NextStart(Vec<&'static str>),
    /// What would be taken into use by logging out and in again.
    LogOut(Vec<&'static str>),
    /// What cannot be in use without a restart.
    Restart(Vec<&'static str>),
}

impl Needs {
    /// The sentence torqcli, the applet and TorqOS Settings all say when something is asked of the person.
    pub fn line(&self) -> Option<String> {
        match self {
            Needs::Nothing | Needs::NextStart(_) => None,
            Needs::LogOut(what) => Some(format!("Log out and back in to use the new {}.", and(what))),
            Needs::Restart(what) => Some(format!("Restart to use the new {}.", and(what))),
        }
    }

    /// What is ready for the next start: nothing to do about it.
    pub fn note(&self) -> Option<String> {
        match self {
            Needs::NextStart(what) => Some(format!("A new {} is installed: it takes effect the next time this machine starts.", and(what))),
            _ => None,
        }
    }
}

fn and(what: &[&str]) -> String {
    match what {
        [] => "system".into(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What `current` (the running system) needs, against `booted` (the system the machine started) and, for a
/// desktop session, `session` (the system the session started on, as the applet recorded it).
pub fn needs(booted: &std::path::Path, current: &std::path::Path, session: Option<&std::path::Path>) -> Needs {
    let link = |sys: &std::path::Path, what: &str| std::fs::read_link(sys.join(what)).ok();
    // The desktop turned on while the machine runs without it: its login screen starts at the next boot.
    if desktop(current).is_some() && desktop(booted).is_none() {
        return Needs::Restart(vec!["desktop"]);
    }
    if let Some(s) = session {
        let mut what = vec![];
        if desktop(s) != desktop(current) {
            what.push("COSMIC");
        }
        if driver(s) != driver(current) {
            what.push("graphics driver");
        }
        if !what.is_empty() {
            return Needs::LogOut(what);
        }
    }
    // Only a new kernel, or a module added to the running one, waits for a start. A boot image and firmware are
    // not news: the first is read only at boot, and firmware when a driver loads.
    if link(booted, "kernel") != link(current, "kernel") {
        return Needs::NextStart(vec!["Linux kernel"]);
    }
    if link(booted, "kernel-modules") != link(current, "kernel-modules") {
        return Needs::NextStart(vec!["kernel module"]);
    }
    Needs::Nothing
}

/// The graphics driver a system links at /run/opengl-driver: its tmpfiles line's target.
fn driver(sys: &std::path::Path) -> Option<String> {
    let lines = std::fs::read_to_string(sys.join("etc/tmpfiles.d/graphics-driver.conf")).ok()?;
    let line = lines.lines().find(|l| l.contains("'/run/opengl-driver'"))?;
    line.split_whitespace().last().filter(|t| t.starts_with("/nix/store/")).map(String::from)
}

/// What only a new login takes up: COSMIC's compositor and its session, by their store paths; None without a desktop.
fn desktop(sys: &std::path::Path) -> Option<Vec<std::path::PathBuf>> {
    ["cosmic-comp", "cosmic-session"]
        .iter()
        .map(|b| std::fs::canonicalize(sys.join("sw/bin").join(b)).ok())
        .collect()
}

#[cfg(test)]
mod needs_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    /// A system: its boot files at `boot`'s version, its driver, and COSMIC at `cosmic` (none: no desktop).
    fn system(root: &Path, name: &str, boot: &str, drv: &str, cosmic: Option<&str>) -> std::path::PathBuf {
        let s = root.join(name);
        std::fs::create_dir_all(s.join("sw/bin")).unwrap();
        for what in ["kernel", "initrd", "kernel-modules", "firmware"] {
            symlink(format!("/nix/store/{boot}-{what}"), s.join(what)).unwrap();
        }
        // As NixOS writes it.
        std::fs::create_dir_all(s.join("etc/tmpfiles.d")).unwrap();
        std::fs::write(s.join("etc/tmpfiles.d/graphics-driver.conf"), format!("'L+' '/run/opengl-driver' '-' '-' '-' '-' /nix/store/{drv}-graphics-drivers\n'r' '/run/opengl-driver-32' '-' '-' '-' '-' \n")).unwrap();
        if let Some(v) = cosmic {
            for b in ["cosmic-comp", "cosmic-session"] {
                let target = root.join(format!("{v}-{b}"));
                std::fs::write(&target, "").unwrap();
                symlink(&target, s.join("sw/bin").join(b)).unwrap();
            }
        }
        s
    }

    #[test]
    fn what_a_switch_needs() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let old = system(r, "old", "a", "m1", Some("c1"));
        assert_eq!(needs(&old, &old, Some(&old)), Needs::Nothing);
        // What the laptop did: only the boot image differs (a host name, zram). Nothing is asked for.
        let image = system(r, "image", "a", "m1", Some("c1"));
        std::fs::remove_file(image.join("initrd")).unwrap();
        symlink("/nix/store/other-initrd", image.join("initrd")).unwrap();
        assert_eq!(needs(&old, &image, Some(&old)), Needs::Nothing, "a boot image is only read at boot");
        // A new COSMIC: the next login; with no session to compare, nothing a person can do here.
        let cosmic = system(r, "cosmic", "a", "m1", Some("c2"));
        assert_eq!(needs(&old, &cosmic, Some(&old)), Needs::LogOut(vec!["COSMIC"]));
        assert_eq!(needs(&old, &cosmic, None), Needs::Nothing);
        assert_eq!(needs(&old, &cosmic, Some(&cosmic)), Needs::Nothing, "a session started on it already uses it");
        // A new graphics driver: the compositor has the old one loaded until the next login.
        let driver = system(r, "driver", "a", "m2", Some("c1"));
        assert_eq!(needs(&old, &driver, Some(&old)), Needs::LogOut(vec!["graphics driver"]));
        // A new kernel: for the next start, and nothing is asked. Never a restart.
        let kernel = system(r, "kernel", "b", "m1", Some("c1"));
        assert_eq!(needs(&old, &kernel, Some(&old)), Needs::NextStart(vec!["Linux kernel"]));
        assert_eq!(Needs::NextStart(vec!["Linux kernel"]).line(), None, "said, not asked for");
        assert!(Needs::NextStart(vec!["Linux kernel"]).note().unwrap().contains("next time this machine starts"));
        // The one case: the desktop turned on, from a machine booted without it.
        let console = system(r, "console", "a", "m1", None);
        assert_eq!(needs(&console, &old, None), Needs::Restart(vec!["desktop"]));
        assert_eq!(Needs::LogOut(vec!["COSMIC", "graphics driver"]).line().unwrap(), "Log out and back in to use the new COSMIC and graphics driver.");
    }
}

/// The machine at a glance, for COSMIC's panel (ADR-049): each part the command line's own.
#[derive(Debug, Clone)]
pub struct Glance {
    pub version: String,
    /// Nothing is wrong: the machine is as its history says.
    pub well: bool,
    /// The first thing `show system alarms` says is wrong, when something is.
    pub alarm: Option<String>,
    /// A commit confirmed waiting (`commit` confirms it).
    pub confirm: Option<Confirm>,
    /// The person's draft differs from the configuration in effect.
    pub draft: bool,
    /// What the running system needs before it is all in use.
    pub needs: Needs,
    /// Proposals waiting for a person, and how many of them are tainted.
    pub waiting: (usize, usize),
    /// The latest change that took effect, in the log's words.
    pub last: Option<String>,
}

/// A commit confirmed waiting.
#[derive(Debug, Clone, PartialEq)]
pub struct Confirm {
    pub seq: u64,
    /// Its deadline, in local time.
    pub deadline: String,
    /// Its deadline, seconds since the epoch, for a countdown.
    pub at: Option<i64>,
}

/// Seconds since the epoch, now.
pub fn now() -> i64 {
    history::Clock::system().now
}

/// `session`: the system the desktop session started on, for what a log-out would take into use.
/// `draft`: `Console::uncommitted`, as the caller asked it.
pub fn glance(session: Option<&std::path::Path>, draft: bool) -> Result<Glance> {
    let cli = cli();
    let (v, report) = crate::status_report(&cli, None, crate::run::watch_log()?)?;
    let well = matches!(report.fix, status::Fix::Nothing) && report.exit == 0;
    let alarm = if well { None } else { report.text.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from) };
    let p = &v["pending_confirm"];
    let clock = history::Clock::system();
    let confirm = p["applied"].as_u64().map(|seq| {
        let ts = p["deadline"].as_str().unwrap_or("?");
        Confirm { seq, deadline: history::one_line(&history::local_stamp(ts, &clock), 40), at: history::parse_ts(ts) }
    });
    let entries = crate::read_ledger(&cli)?;
    let waiting = waiting_on_me(&entries, &crate::me().unwrap_or_default());
    let needs = needs(std::path::Path::new("/run/booted-system"), std::path::Path::new("/run/current-system"), session);
    Ok(Glance {
        version: version(),
        well,
        alarm,
        confirm,
        draft,
        needs,
        waiting: (waiting.len(), waiting.iter().filter(|e| e.tainted).count()),
        last: history::last_change(&entries),
    })
}



// ── What a session started on ────────────────────────────────────────────────────────────────────

/// Where this login's system is recorded: the login's runtime directory outlives a logout while an SSH
/// session is open, so the name is the login's own.
fn session_file() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    let id = std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "desktop".into());
    Some(PathBuf::from(dir).join(format!("torq-session-system-{id}")))
}

/// The system this login's desktop started on, if the applet has recorded one.
pub fn session_recorded() -> Option<PathBuf> {
    std::fs::read_link(session_file()?).ok()
}

/// The system this login's desktop started on, recorded the first time it is asked (the applet, which
/// starts with the session), so an applet restarted mid-session still knows it.
pub fn session_system() -> Option<PathBuf> {
    if let Some(s) = session_recorded() {
        return Some(s);
    }
    let now = std::fs::read_link("/run/current-system").ok()?;
    if let Some(f) = session_file() {
        let _ = std::os::unix::fs::symlink(&now, f);
    }
    Some(now)
}

// ── The command line, in a window ────────────────────────────────────────────────────────────────

pub use crate::complete::Choice;
pub use crate::session::Mode;

/// Words as one line of a command, quoted where a word needs it.
pub fn line(words: &[&str]) -> String {
    words.iter().map(|w| crate::syntax::quote(w)).collect::<Vec<_>>().join(" ")
}

/// Which of torqcli's sessions a line runs in. The draft is the person's own saved candidate, shared with
/// their terminal's shell.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum At {
    /// What the pages read: operational mode.
    Read,
    /// What the pages change: configuration mode, from the top.
    Draft,
    /// The Commands page's: whichever mode the person has gone to.
    Typed,
    /// What only asks whether the draft has changes: never held up by a `commit check`.
    Watch,
}

/// What a typed line did: its output or why not, and where the session is now.
#[derive(Debug, Clone)]
pub struct Typed {
    pub output: Result<String, String>,
    pub configuration: bool,
    pub edit: Vec<String>,
}

/// Where a command's questions go, each with the way back for its answer.
type Asks = std::sync::mpsc::Sender<(String, std::sync::mpsc::Sender<bool>)>;

enum Job {
    Run(At, String, std::sync::mpsc::Sender<Result<String, String>>, Option<Asks>),
    Typed(String, std::sync::mpsc::Sender<Typed>, Option<Asks>),
    Choices(At, String, std::sync::mpsc::Sender<Vec<Choice>>),
    Uncommitted(std::sync::mpsc::Sender<bool>),
}

/// torqcli's own sessions, on threads of their own (a session is not to be shared between threads, and a
/// `commit check` must not hold up what a page reads). Cloned freely; each call blocks its caller.
#[derive(Clone)]
pub struct Console {
    reads: std::sync::mpsc::Sender<Job>,
    edits: std::sync::mpsc::Sender<Job>,
    watch: std::sync::mpsc::Sender<Job>,
}

impl Default for Console {
    fn default() -> Self {
        Self::start()
    }
}

impl Console {
    pub fn start() -> Self {
        let spawn = |sessions: &'static [At]| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || serve(sessions, rx));
            tx
        };
        Console { reads: spawn(&[At::Read]), edits: spawn(&[At::Draft, At::Typed]), watch: spawn(&[At::Watch]) }
    }

    /// Only what the panel needs: whether the draft has changes.
    pub fn watcher() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || serve(&[At::Watch], rx));
        let none = || std::sync::mpsc::channel().0;
        Console { reads: none(), edits: none(), watch: tx }
    }

    fn queue(&self, at: At) -> &std::sync::mpsc::Sender<Job> {
        match at {
            At::Read => &self.reads,
            At::Draft | At::Typed => &self.edits,
            At::Watch => &self.watch,
        }
    }

    /// Run a line, its output cleaned as the terminal's is.
    pub fn run(&self, at: At, line: &str) -> Result<String, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.queue(at).send(Job::Run(at, line.to_string(), tx, None)).map_err(|_| GONE.to_string())?;
        rx.recv().unwrap_or_else(|_| Err(GONE.to_string()))
    }

    /// Run a line that may ask a person something: `ask` answers each question.
    pub fn act(&self, at: At, line: &str, ask: impl Fn(String) -> bool + Send) -> Result<String, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let (asks, questions) = std::sync::mpsc::channel();
        self.queue(at).send(Job::Run(at, line.to_string(), tx, Some(asks))).map_err(|_| GONE.to_string())?;
        std::thread::scope(|scope| {
            scope.spawn(move || answering(questions, ask));
            rx.recv().unwrap_or_else(|_| Err(GONE.to_string()))
        })
    }

    /// A line typed on the Command Line: `ask` answers what it asks.
    pub fn typed(&self, line: &str, ask: impl Fn(String) -> bool + Send) -> Typed {
        let (tx, rx) = std::sync::mpsc::channel();
        let (asks, questions) = std::sync::mpsc::channel();
        let gone = || Typed { output: Err(GONE.to_string()), configuration: false, edit: vec![] };
        if self.edits.send(Job::Typed(line.to_string(), tx, Some(asks))).is_err() {
            return gone();
        }
        std::thread::scope(|scope| {
            scope.spawn(move || answering(questions, ask));
            rx.recv().unwrap_or_else(|_| gone())
        })
    }

    /// What may come next after `before` (the line up to the cursor): `?` and Tab's answer.
    pub fn choices(&self, at: At, before: &str) -> Vec<Choice> {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.queue(at).send(Job::Choices(at, before.to_string(), tx)).is_err() {
            return vec![];
        }
        rx.recv().unwrap_or_default()
    }

    /// Whether the person's draft differs from the configuration in effect.
    pub fn uncommitted(&self) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        if self.watch.send(Job::Uncommitted(tx)).is_err() {
            return false;
        }
        rx.recv().unwrap_or(false)
    }
}

const GONE: &str = "torqcli's session in this window has stopped";

/// Lines cleaned of anything that could fool the eye: the ledger's reasons are not ours.
fn cleaned(out: String) -> String {
    out.lines().map(|l| format!("{}\n", history::clean(l))).collect()
}

/// A worker: its sessions, and the jobs for them, in order.
fn serve(sessions: &'static [At], jobs: std::sync::mpsc::Receiver<Job>) {
    let cli = cli();
    // What a command asks a person goes to whoever gave this job its `Asks`; with none, there is nobody to ask.
    let asks: std::rc::Rc<std::cell::RefCell<Option<Asks>>> = Default::default();
    let asking = asks.clone();
    let real = crate::shell::Real::asking(
        &cli,
        Box::new(move |question| {
            use crate::session::Answer;
            let Some(tx) = asking.borrow().clone() else { return Answer::NobodyToAsk };
            let (back, answer) = std::sync::mpsc::channel();
            if tx.send((cleaned(question.to_string()), back)).is_err() {
                return Answer::NobodyToAsk;
            }
            match answer.recv() {
                Ok(true) => Answer::Yes,
                Ok(false) => Answer::No,
                Err(_) => Answer::NobodyToAsk,
            }
        }),
    );
    let session = |at: At| open_session(&cli, &real, Some(crate::candidate::Candidate::default_path()?), at);
    let mut made: Vec<(At, Result<crate::session::Session<'_>, String>)> = sessions.iter().map(|&at| (at, session(at).map_err(|e| said(&e)))).collect();
    for job in jobs {
        match job {
            Job::Run(at, line, reply, question) => {
                *asks.borrow_mut() = question;
                let r = match of(&mut made, at) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
                        s.exec(&line).map(cleaned).map_err(|e| said(&e))
                    }
                    Some(Err(e)) => Err(e.clone()),
                    None => Err(GONE.to_string()),
                };
                *asks.borrow_mut() = None;
                let _ = reply.send(r);
            }
            Job::Typed(line, reply, question) => {
                *asks.borrow_mut() = question;
                let t = match of(&mut made, At::Typed) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
                        let output = match refused_here(s.mode, &line) {
                            Some(why) => Err(why.to_string()),
                            None => s.exec(&line).map(cleaned).map_err(|e| said(&e)),
                        };
                        // `exit` at the top of operational mode: the window stays, and so does the session.
                        s.done = false;
                        Typed { output, configuration: s.mode == Mode::Configuration, edit: s.edit_path().to_vec() }
                    }
                    Some(Err(e)) => Typed { output: Err(e.clone()), configuration: false, edit: vec![] },
                    None => Typed { output: Err(GONE.to_string()), configuration: false, edit: vec![] },
                };
                *asks.borrow_mut() = None;
                let _ = reply.send(t);
            }
            Job::Choices(at, before, reply) => {
                let c = match of(&mut made, at) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
                        choices_of(s, &before)
                    }
                    _ => vec![],
                };
                let _ = reply.send(c);
            }
            Job::Uncommitted(reply) => {
                let u = match of(&mut made, At::Watch) {
                    Some(Ok(s)) => {
                        s.reread();
                        s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
                        s.uncommitted().unwrap_or(false)
                    }
                    _ => false,
                };
                let _ = reply.send(u);
            }
        }
    }
}

/// The complete words before the cursor, and the word being typed; None inside an open quote.
fn at_cursor(before: &str) -> Option<(Vec<String>, String)> {
    crate::session::words(before).ok()?;
    let partial = if before.ends_with(char::is_whitespace) { "" } else { before.rsplit(char::is_whitespace).next().unwrap_or("") };
    let done = &before[..before.len() - partial.len()];
    Some((crate::session::words(done).ok()?, partial.to_string()))
}

fn choices_of(s: &mut crate::session::Session<'_>, before: &str) -> Vec<Choice> {
    let Some((words, partial)) = at_cursor(before) else { return vec![] };
    let at = crate::complete::At {
        mode: s.mode,
        edit: s.edit_path().to_vec(),
        data: s.help_data(),
        configurations: crate::complete::Later::new(Vec::new),
        files: s.nix_files(),
        interfaces: vec![],
    };
    crate::complete::choices_at(&crate::complete::Know { schema: &s.schema, tree: &s.tree }, &at, &words, &partial)
}

/// Whether a line must go to a terminal: it has a person type a new secret, or opens their editor. A yes or no
/// is asked in the window, and an approval in COSMIC's own dialog (ADR-045 5), never a password in a window
/// TorqOS draws.
pub fn needs_terminal(mode: Mode, line: &str) -> bool {
    fn types(op: &crate::syntax::Op) -> bool {
        use crate::syntax::{FileAction, LoadFrom, Op, Request};
        match op {
            Op::Request(Request::Password { .. } | Request::BackupKey { .. }) => true,
            Op::File { action: FileAction::Edit, .. } => true,
            Op::Load { from: LoadFrom::Terminal, .. } => true,
            Op::Run(op) => types(op),
            Op::Top { then: Some(op) } => types(op),
            _ => false,
        }
    }
    crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(mode, &w).ok()).is_some_and(|l| types(&l.op))
}

/// What the window will not do, and why: a line that would hold the shared draft for this window alone,
/// and shut out its own pages and the terminal a commit goes to.
pub fn refused_here(mode: Mode, line: &str) -> Option<&'static str> {
    let op = crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(mode, &w).ok())?.op;
    matches!(op, crate::syntax::Op::Configure { exclusive: true }).then_some("configure exclusive holds the draft for one session: use it in a terminal, where it is yours alone")
}

/// Whether a line only loads something into the draft, however long that takes: it runs with the draft's
/// session, from configuration mode (there an update only loads), so that what the pages read is not held up.
pub fn loads_draft(line: &str) -> bool {
    use crate::syntax::{Op, Request};
    crate::session::words(line).ok().and_then(|w| crate::grammar::read_line(Mode::Operational, &w).ok()).is_some_and(|l| matches!(l.op, Op::Request(Request::Update { .. } | Request::Pull | Request::SoftwareRollback { .. })))
}

/// Run `script` (torqcli's `-c`) in COSMIC's terminal, and wait until it is closed: `draft`, on the person's
/// saved candidate. It stays open at the end, for what happened to be read. Whether the script succeeded.
pub fn in_terminal(script: &str, draft: bool) -> Result<bool, String> {
    let run = if draft { "torqcli --candidate -c \"$1\"" } else { "torqcli -c \"$1\"" };
    let shell = format!("{run}; echo $? > \"$2\"; echo; read -r -p 'Press Enter to close. ' _");
    let dir = tempfile::tempdir().map_err(|e| format!("making a private folder: {e}"))?;
    let status = dir.path().join("status");
    std::process::Command::new("cosmic-term")
        // Not a daemon: it would return at once, with the commit still asking.
        .args(["--no-daemon", "-e", "sh", "-c", &shell, "sh", script])
        .arg(&status)
        .status()
        .map_err(|e| format!("opening COSMIC's terminal: {e}"))?;
    Ok(std::fs::read_to_string(&status).is_ok_and(|s| s.trim() == "0"))
}

/// A line as `-c` reads it, each word quoted: a `;` inside a typed word must not split the script.
fn requoted(line: &str) -> String {
    match crate::session::words(line) {
        Ok(w) => w.iter().map(|x| if x == "|" { x.clone() } else { crate::syntax::quote(x) }).collect::<Vec<_>>().join(" "),
        Err(_) => line.to_string(),
    }
}

/// The script `in_terminal` runs for a line of the given mode: in configuration mode the draft's, at the
/// place (`edit`) the line was typed at, whose meaning depends on it.
pub fn terminal_script(mode: Mode, edit: &[String], line: &str) -> (String, bool) {
    let line = requoted(line);
    match mode {
        Mode::Configuration if edit.is_empty() => (format!("configure; {line}"), true),
        Mode::Configuration => {
            let place: Vec<&str> = std::iter::once("edit").chain(edit.iter().map(String::as_str)).collect();
            (format!("configure; {}; {line}", self::line(&place)), true)
        }
        Mode::Operational => (line, false),
    }
}

// ── The pages that are the command line's output ─────────────────────────────────────────────────

/// How a button acts on its command.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum How {
    /// Run it (in a terminal, if it asks: `needs_terminal`).
    Run,
    /// Open the Commands page with it typed so far, to finish: operational mode.
    FillOp,
    /// The same, in configuration mode.
    FillCf,
}

/// A button: its label, the torqcli command, and how it acts.
#[derive(Clone, Copy, Debug)]
pub struct Action {
    pub label: &'static str,
    pub command: &'static str,
    pub how: How,
}

/// A page that shows commands' output and offers actions: nothing on it is the window's own.
#[derive(Clone, Copy, Debug)]
pub struct View {
    /// Its name on the command line of the application (`torq-settings data`).
    pub id: &'static str,
    pub name: &'static str,
    pub icon: &'static str,
    /// A title, and the command whose output is that section.
    pub sections: &'static [(&'static str, &'static str)],
    pub actions: &'static [Action],
}

const fn run(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::Run }
}
const fn op(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::FillOp }
}
const fn cf(label: &'static str, command: &'static str) -> Action {
    Action { label, command, how: How::FillCf }
}

/// What the agent may do without asking, and every button on the Proposals page besides each proposal's own.
pub const AGENT: &[Action] = &[op("Trust the agent for a while…", "request system agent trust "), run("End that trust now", "request system agent trust revoke")];

pub static VIEWS: &[View] = &[
    View {
        id: "software",
        name: "Software",
        icon: "software-update-available-symbolic",
        sections: &[("What this machine runs", "show version")],
        actions: &[
            run("Load the newest TorqOS into the draft", "request system update"),
            run("Load everything newer, nixpkgs too", "request system update all"),
            op("Load an earlier configuration's software…", "request system software rollback "),
            op("Search the software you can install…", "show packages "),
            cf("Add a package for everyone…", "set environment systemPackages "),
            cf("Add a package for one person…", "set users users "),
            cf("Remove a package…", "delete environment systemPackages "),
        ],
    },
    View {
        id: "services",
        name: "Services",
        icon: "network-server-symbolic",
        sections: &[("Turned on by the configuration", "show services"), ("The network", "show interfaces")],
        actions: &[cf("Turn a service on or change it…", "set services "), cf("Turn a service off…", "delete services ")],
    },
    View {
        id: "data",
        name: "Data",
        icon: "drive-harddisk-system-symbolic",
        sections: &[("Snapshots, each from before a change", "show system snapshots"), ("Backups", "show system backups"), ("Disks", "show system storage")],
        actions: &[
            run("Take a snapshot now", "request system snapshot take"),
            op("Put a path back from a snapshot…", "request system snapshot restore "),
            op("Delete a snapshot…", "request system snapshot delete "),
            cf("Declare a backup…", "set services restic backups "),
            op("Give a backup its key…", "request system backup key "),
            op("Put a path back from a backup…", "request system restore "),
        ],
    },
    View {
        id: "machine",
        name: "This Machine",
        icon: "computer-symbolic",
        sections: &[("How long it has run", "show system uptime"), ("People", "show users"), ("Nix files of the configuration", "file list")],
        actions: &[
            run("Change your password", "request system password"),
            op("Change someone's password…", "request system password "),
            run("Return the machine to its history", "request system repair"),
            run("Say no more of a ledger disagreement", "request system ledger acknowledge"),
            run("Cancel the commit scheduled", "clear system commit"),
        ],
    },
];

#[cfg(test)]
mod page_tests {
    use super::*;
    use crate::grammar::{read_line, walk};
    use crate::session::words;

    fn whole(mode: Mode, c: &str) {
        read_line(mode, &words(c).unwrap()).unwrap_or_else(|e| panic!("`{c}` is not a command of torqcli: {e:#}"));
    }

    /// Every action the window offers is a torqcli command (ADR-045 2): the grammar's own reading of it.
    #[test]
    fn everything_the_pages_offer_is_a_command() {
        let check = |a: &Action| match a.how {
            How::Run => whole(Mode::Operational, a.command),
            How::FillOp => assert!(walk(Mode::Operational, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
            How::FillCf => assert!(walk(Mode::Configuration, &words(a.command).unwrap()).is_ok(), "`{}`", a.command),
        };
        for v in VIEWS {
            for (_, c) in v.sections {
                whole(Mode::Operational, c);
            }
            v.actions.iter().for_each(check);
        }
        AGENT.iter().for_each(check);
        BACKUP.iter().for_each(check);
        for c in ["show system alarms", "show system ledger", "show system proposals", "show system commit", "show system remote", "request system remote key", "help nginx"] {
            whole(Mode::Operational, c);
        }
        for c in ["set torq remote url git@github.com:a/b.git", "show", "show | compare", "commit check", "commit comment \"x\" confirmed 5", "commit at 12:00", "rollback 0", "rollback 3", "set a b", "delete a b"] {
            whole(Mode::Configuration, c);
        }
        whole(Mode::Operational, "request system revert 5");
        whole(Mode::Operational, "request system apply 5");
        whole(Mode::Configuration, "commit");
    }

    /// Only what has a person type a secret, or edit a file, goes to a terminal.
    #[test]
    fn what_goes_to_a_terminal() {
        let t = |m, c| needs_terminal(m, c);
        for c in ["file edit a.nix", "load merge terminal", "run request system password", "top load set terminal"] {
            assert!(t(Mode::Configuration, c), "{c}");
        }
        for c in ["request system password", "request system password sam", "request system backup key b"] {
            assert!(t(Mode::Operational, c), "{c}");
        }
        for c in ["commit", "commit comment \"x\"", "commit confirmed 5", "commit at 12:00", "commit check", "set a b", "delete", "top delete", "rollback 1", "show | compare", "run request system repair"] {
            assert!(!t(Mode::Configuration, c), "{c}: asked in the window, approved in the dialog");
        }
        for c in ["request system revert 5", "request system apply 5", "request system repair", "request system snapshot take", "clear system commit", "show system alarms", "request system update"] {
            assert!(!t(Mode::Operational, c), "{c}");
        }
    }

    #[test]
    fn a_line_is_quoted_where_it_must_be() {
        assert_eq!(line(&["set", "a", "b c"]), "set a \"b c\"");
    }

    #[test]
    fn the_terminal_runs_the_draft_from_configuration_mode() {
        assert_eq!(terminal_script(Mode::Configuration, &[], "commit"), ("configure; commit".to_string(), true));
        // A bare `delete` at a place deletes that place: never everything.
        assert_eq!(terminal_script(Mode::Configuration, &["services".into(), "openssh".into()], "delete"), ("configure; edit services openssh; delete".to_string(), true));
        // A `;` in a word stays in the word.
        assert_eq!(terminal_script(Mode::Configuration, &[], "commit comment \"a;b\""), ("configure; commit comment \"a;b\"".to_string(), true));
        assert_eq!(terminal_script(Mode::Operational, &[], "request system repair"), ("request system repair".to_string(), false));
    }
}

/// One of a worker's sessions.
fn of<'b, 's>(made: &'b mut [(At, Result<crate::session::Session<'s>, String>)], at: At) -> Option<&'b mut Result<crate::session::Session<'s>, String>> {
    made.iter_mut().find(|(a, _)| *a == at).map(|(_, s)| s)
}

/// A proposal waiting for a person.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub seq: u64,
    pub reason: String,
    pub by: String,
    pub tainted: bool,
}

/// Proposals waiting for a person: the agent's and other people's. One's own commit that did not go through leaves
/// a proposal too, and that is not something waiting on them.
fn waiting_on_me<'a>(entries: &'a [history::Entry], me: &str) -> Vec<&'a history::Entry> {
    history::waiting_proposals(entries).into_iter().filter(|e| e.data["caller"].as_str().unwrap_or(&e.actor) != me).collect()
}

/// The proposals waiting for a person.
pub fn proposals() -> Result<Vec<Proposal>> {
    let entries = crate::read_ledger(&cli())?;
    Ok(waiting_on_me(&entries, &crate::me()?)
        .into_iter()
        .map(|e| Proposal { seq: e.seq, reason: history::one_line(&e.reason, 300), by: history::one_line(&e.actor, 40), tainted: e.tainted })
        .collect())
}

/// `show system commit`'s configurations: each number (for `rollback <n>`) and its line.
pub fn configurations(shown: &str) -> Vec<(u32, String)> {
    shown
        .lines()
        .filter_map(|l| {
            let (n, rest) = l.trim().split_once(char::is_whitespace)?;
            Some((n.parse().ok()?, rest.trim().to_string()))
        })
        .collect()
}

/// COSMIC's own dialog for restarting: it asks first, and counts down.
pub fn ask_restart() {
    spawn("cosmic-osd", &["restart"]);
}

/// COSMIC's own dialog for logging out: it asks first, and counts down.
pub fn ask_log_out() {
    spawn("cosmic-osd", &["log-out"]);
}

/// TorqOS Settings, at a page (`VIEWS`' ids, or overview, configuration, changes, history, proposals, commands).
pub fn open_settings(page: &str) {
    spawn("torq-settings", &[page]);
}

fn spawn(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program).args(args).spawn();
}

#[cfg(test)]
#[test]
fn the_configurations_shown_with_their_numbers() {
    let shown = "Rollback  Change  When  By  What\n0         #16     then  me  One\n1         #14     then  me  Two\n";
    assert_eq!(configurations(shown), vec![(0, "#16     then  me  One".to_string()), (1, "#14     then  me  Two".to_string())]);
}

#[cfg(test)]
#[test]
fn the_window_keeps_what_would_lock_it_out() {
    assert!(refused_here(Mode::Operational, "configure exclusive").is_some());
    assert!(refused_here(Mode::Operational, "configure").is_none());
    assert!(loads_draft("request system update") && loads_draft("request system pull"));
    assert!(!loads_draft("show version") && !loads_draft("request system repair"));
}

// ── What the window's tree and diff viewer read ──────────────────────────────────────────────────

/// The settings of a configuration, from its `| display set` lines: each one's path and its value.
/// A list has one line per value.
pub fn settings_of(shown: &str) -> Vec<(Vec<String>, String)> {
    shown
        .lines()
        .filter_map(|l| {
            let mut w = crate::session::words(l.strip_prefix("set ")?).ok()?;
            let value = w.pop()?;
            (!w.is_empty()).then_some((w, value))
        })
        .collect()
}

/// A change's configuration number, from `show system commit`'s lines (0 is the one in effect); None: it
/// made no configuration.
pub fn rollback_of(shown: &str, seq: u64) -> Option<u32> {
    let wanted = format!("#{seq}");
    configurations(shown).into_iter().find(|(_, rest)| rest.split_whitespace().next() == Some(wanted.as_str())).map(|(n, _)| n)
}

/// What a proposal does, as the broker had git write it when it was proposed: never the proposer's own
/// text, which can say one thing while git does another (ADR-041).
pub fn proposal_effect(seq: u64) -> Result<String> {
    let cli = cli();
    let log = crate::run::watch_log()?;
    let v = match crate::run::broker_json(crate::broker(&cli, &["show", &seq.to_string()])?, &log)? {
        crate::run::Outcome::Done(v) => v,
        crate::run::Outcome::Busy => anyhow::bail!("another change is being made right now; try again in a moment"),
        crate::run::Outcome::Refused(m) | crate::run::Outcome::Broken(m) => anyhow::bail!("{}", history::one_line(&m, 300)),
    };
    Ok(cleaned(v["data"]["effect"].as_str().unwrap_or_default().to_string()))
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    #[test]
    fn a_configuration_read_as_settings() {
        let shown = "set networking hostName torqtop\nset environment systemPackages htop\nset environment systemPackages jq\nset users users casey description \"Casey D\"\n## software: torq 2026\n";
        let s = settings_of(shown);
        assert_eq!(s[0], (vec!["networking".to_string(), "hostName".to_string()], "torqtop".to_string()));
        assert_eq!((s[1].1.as_str(), s[2].1.as_str()), ("htop", "jq"), "a list, a line for each");
        assert_eq!(s[3].1, "Casey D", "a value with a space is one value");
        assert_eq!(s.len(), 4, "a comment is not a setting");
    }

    #[test]
    fn a_change_finds_its_configuration() {
        let shown = "Rollback  Change  When  By  What\n0         #16     then  me  One\n1         #14     then  me  Two\n";
        assert_eq!(rollback_of(shown, 16), Some(0));
        assert_eq!(rollback_of(shown, 14), Some(1));
        assert_eq!(rollback_of(shown, 1), None, "not #1 of #14 or #16");
    }
}

/// A session of torqcli's on this machine: `saved`, the candidate it works on (None: its own).
fn open_session<'a>(cli: &Cli, machine: &'a dyn crate::session::Machine, saved: Option<PathBuf>, at: At) -> Result<crate::session::Session<'a>> {
    use anyhow::Context as _;
    let schema = crate::tree::Schema::parse(&std::fs::read_to_string(crate::shell::SCHEMA).with_context(|| format!("reading {} (is this TorqOS?)", crate::shell::SCHEMA))?)?;
    let tree = crate::nixos::Tree::parse(&std::fs::read_to_string(crate::shell::NIXOS_SCHEMA).with_context(|| format!("reading {}", crate::shell::NIXOS_SCHEMA))?)?;
    let who = crate::me()?;
    let mut s = crate::session::Session::new(schema, tree, crate::candidate::Repo::new(&cli.repo), saved, machine, &who);
    s.follow_schemas(std::path::Path::new(crate::shell::SCHEMA), std::path::Path::new(crate::shell::NIXOS_SCHEMA));
    s.cache = crate::evalcache::Cache::personal();
    if at == At::Draft {
        s.exec("configure")?;
    }
    Ok(s)
}

/// Keep a commit confirmed waiting: `commit` of an empty candidate, a private one, always, so that the
/// person's draft is never swept in with it.
pub fn confirm() -> Result<String> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Draft)?;
    Ok(cleaned(s.exec("commit")?))
}

/// A job's questions, each answered in turn, until the job has no more.
fn answering(questions: std::sync::mpsc::Receiver<(String, std::sync::mpsc::Sender<bool>)>, ask: impl Fn(String) -> bool) {
    for (question, back) in questions {
        let _ = back.send(ask(question));
    }
}

// ── What a switch can bring in without a new login ───────────────────────────────────────────────

/// The parts of COSMIC its session starts again by itself, new from the system's path, each with the programs
/// whose change means it is out of date. Not the compositor or the session: those are the login. Not the
/// greeter, which holds the lock screen. The panel is last: this applet lives in it.
const LIVE_PARTS: &[(&str, &[&str])] = &[
    ("cosmic-settings-daemon", &["cosmic-settings-daemon"]),
    ("cosmic-notifications", &["cosmic-notifications"]),
    ("cosmic-app-library", &["cosmic-app-library"]),
    ("cosmic-launcher", &["cosmic-launcher", "pop-launcher"]),
    ("cosmic-workspaces", &["cosmic-workspaces"]),
    ("cosmic-osd", &["cosmic-osd"]),
    ("cosmic-osk", &["cosmic-osk"]),
    ("cosmic-bg", &["cosmic-bg"]),
    ("cosmic-idle", &["cosmic-idle"]),
    ("cosmic-files-applet", &["cosmic-files-applet"]),
    ("cosmic-panel", &["cosmic-panel", "cosmic-panel-button", "cosmic-applet-audio", "torq-applet"]),
];

/// The live parts of the desktop `new` has other programs for than `old`.
pub fn stale_parts(old: &std::path::Path, new: &std::path::Path) -> Vec<&'static str> {
    let of = |sys: &std::path::Path, b: &str| std::fs::canonicalize(sys.join("sw/bin").join(b)).ok();
    LIVE_PARTS.iter().filter(|(_, programs)| programs.iter().any(|b| of(old, b) != of(new, b))).map(|(part, _)| *part).collect()
}

/// This person's processes, each with its command line when that is one word, as COSMIC's session starts its parts.
fn my_parts() -> Vec<(i32, String)> {
    let me = rustix::process::geteuid().as_raw();
    let Ok(procs) = std::fs::read_dir("/proc") else { return vec![] };
    procs
        .flatten()
        .filter_map(|e| {
            use std::os::unix::fs::MetadataExt as _;
            let pid: i32 = e.file_name().to_str()?.parse().ok()?;
            (e.metadata().ok()?.uid() == me).then_some(())?;
            let cmdline = std::fs::read(e.path().join("cmdline")).ok()?;
            // One word, as the session started it.
            let word = cmdline.strip_suffix(&[0])?;
            (!word.contains(&0)).then(|| Some((pid, String::from_utf8_lossy(word).into_owned())))?
        })
        .collect()
}

/// End each of these programs of this person's, by its exact command line: COSMIC's session starts it again.
pub fn restart_parts(parts: &[&str]) {
    let running = my_parts();
    for part in parts {
        for (pid, _) in running.iter().filter(|(_, c)| c == part) {
            if let Some(pid) = rustix::process::Pid::from_raw(*pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
        }
    }
}

#[cfg(test)]
mod live_parts {
    use super::*;
    use std::os::unix::fs::symlink;

    fn system(root: &std::path::Path, name: &str, programs: &[(&str, &str)]) -> PathBuf {
        let s = root.join(name);
        std::fs::create_dir_all(s.join("sw/bin")).unwrap();
        for (b, version) in programs {
            let target = root.join(format!("{version}-{b}"));
            std::fs::write(&target, "").unwrap();
            symlink(&target, s.join("sw/bin").join(b)).unwrap();
        }
        s
    }

    #[test]
    fn only_what_changed_is_started_again_the_panel_last() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let old = system(r, "old", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "1"), ("cosmic-comp", "1")]);
        assert!(stale_parts(&old, &old).is_empty());
        let applet = system(r, "applet", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "2"), ("cosmic-comp", "1")]);
        assert_eq!(stale_parts(&old, &applet), vec!["cosmic-panel"], "the applet lives in the panel");
        let both = system(r, "both", &[("cosmic-panel", "2"), ("cosmic-bg", "2"), ("torq-applet", "1"), ("cosmic-comp", "1")]);
        assert_eq!(stale_parts(&old, &both), vec!["cosmic-bg", "cosmic-panel"], "the panel last: it holds this applet");
        let comp = system(r, "comp", &[("cosmic-panel", "1"), ("cosmic-bg", "1"), ("torq-applet", "1"), ("cosmic-comp", "2")]);
        assert!(stale_parts(&old, &comp).is_empty(), "the compositor is the login: never ended here");
    }

    #[test]
    fn the_login_is_never_among_them() {
        for (part, _) in LIVE_PARTS {
            assert!(!matches!(*part, "cosmic-comp" | "cosmic-session" | "cosmic-greeter"), "{part}");
        }
        assert_eq!(LIVE_PARTS.last().unwrap().0, "cosmic-panel");
    }
}

// ── The configuration, as a person reads it ──────────────────────────────────────────────────────

/// The areas Settings groups a configuration into, in the order they are listed.
pub const AREAS: &[&str] = &["Network", "Services", "Software", "Desktop", "Boot", "People", "Other"];

/// Which area a setting is in.
pub fn area_of(path: &[String]) -> &'static str {
    match (path.first().map(String::as_str), path.get(1).map(String::as_str)) {
        (Some("networking"), _) => "Network",
        (Some("services" | "virtualisation"), _) => "Services",
        (Some("environment" | "programs" | "nixpkgs"), _) | (Some("system"), Some("software")) => "Software",
        (Some("torq"), Some("desktop")) | (Some("xdg" | "fonts" | "hardware"), _) => "Desktop",
        (Some("boot"), _) => "Boot",
        (Some("users" | "security"), _) => "People",
        _ => "Other",
    }
}

/// A word of a setting's path as a person reads it: `hostName` is "Host name".
pub fn label(word: &str) -> String {
    match word {
        "openssh" => return "SSH".into(),
        "enable" => return "Enabled".into(),
        _ => {}
    }
    let mut parts: Vec<String> = vec![];
    let mut current = String::new();
    let mut lower_before = false;
    for c in word.chars() {
        if c == '-' || c == '_' || c == '.' {
            parts.extend((!current.is_empty()).then(|| std::mem::take(&mut current)));
            lower_before = false;
            continue;
        }
        if c.is_uppercase() && lower_before {
            parts.push(std::mem::take(&mut current));
        }
        lower_before = c.is_lowercase() || c.is_ascii_digit();
        current.push(c);
    }
    parts.extend((!current.is_empty()).then_some(current));
    let shown: Vec<String> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| match p.to_lowercase().as_str() {
            a @ ("ssh" | "efi" | "uefi" | "dns" | "dhcp" | "ip" | "tcp" | "udp" | "api" | "url" | "gpu" | "cpu" | "usb" | "vpn" | "tls" | "gid" | "uid") => a.to_uppercase(),
            "nix" | "nixos" | "nixpkgs" | "cosmic" if p.chars().all(char::is_lowercase) => p.clone(),
            lower if i == 0 => {
                let mut c = lower.chars();
                c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
            }
            lower => lower.to_string(),
        })
        .collect();
    shown.join(" ")
}

/// A setting's card and its row on it: `services openssh enable` is on "SSH", as "Enabled".
pub fn card_and_row(path: &[String]) -> (String, String) {
    let row = |words: &[String]| words.iter().map(|w| label(w)).collect::<Vec<_>>().join(" › ");
    match path {
        [_, group, rest @ ..] if !rest.is_empty() => (label(group), row(rest)),
        [_, last] => ("General".into(), label(last)),
        other => ("General".into(), row(other)),
    }
}

#[cfg(test)]
mod reading_tests {
    use super::*;

    fn path(s: &str) -> Vec<String> {
        s.split(' ').map(String::from).collect()
    }

    #[test]
    fn a_setting_is_in_an_area() {
        for (p, area) in [("networking hostName", "Network"), ("services openssh enable", "Services"), ("system software torq source", "Software"), ("environment systemPackages", "Software"), ("torq desktop enable", "Desktop"), ("boot loader efi canTouchEfiVariables", "Boot"), ("users users casey packages", "People"), ("time timeZone", "Other"), ("torq dev enable", "Other")] {
            assert_eq!(area_of(&path(p)), area, "{p}");
            assert!(AREAS.contains(&area));
        }
    }

    #[test]
    fn names_read_as_words() {
        for (w, shown) in [("hostName", "Host name"), ("timeZone", "Time zone"), ("systemPackages", "System packages"), ("PasswordAuthentication", "Password authentication"), ("canTouchEfiVariables", "Can touch EFI variables"), ("systemd-boot", "Systemd boot"), ("openssh", "SSH"), ("enable", "Enabled"), ("ssh", "SSH")] {
            assert_eq!(label(w), shown);
        }
    }

    #[test]
    fn a_setting_has_a_card_and_a_row() {
        assert_eq!(card_and_row(&path("services openssh enable")), ("SSH".to_string(), "Enabled".to_string()));
        assert_eq!(card_and_row(&path("services openssh settings PasswordAuthentication")), ("SSH".to_string(), "Settings › Password authentication".to_string()));
        assert_eq!(card_and_row(&path("networking hostName")), ("General".to_string(), "Host name".to_string()));
    }
}

// ── Applications ─────────────────────────────────────────────────────────────────────────────────

/// An application, as Flathub names it.
#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub id: String,
    pub name: String,
    pub summary: String,
}

fn flatpak(args: &[&str]) -> Result<String> {
    use anyhow::Context as _;
    let out = std::process::Command::new("flatpak").args(args).stdin(std::process::Stdio::null()).output().context("running flatpak")?;
    if !out.status.success() {
        anyhow::bail!("{}", history::one_line(&String::from_utf8_lossy(&out.stderr), 300));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Lines of `id<TAB>name<TAB>summary`.
fn apps_of(table: &str) -> Vec<App> {
    table
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (id, name) = (f.next()?, f.next()?);
            crate::tree::flatpak_id_ok(id).then(|| App { id: id.to_string(), name: history::one_line(name, 80), summary: history::one_line(f.next().unwrap_or_default(), 200) })
        })
        .collect()
}

/// This person's installed applications.
pub fn installed_apps() -> Result<Vec<App>> {
    let mut apps = apps_of(&flatpak(&["list", "--user", "--app", "--columns=application,name,description"])?);
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

/// What Flathub has for `terms`, best first.
pub fn search_apps(terms: &str) -> Result<Vec<App>> {
    let found = || flatpak(&["search", "--columns=application,name,description", terms]);
    let mut table = found()?;
    // A person new to Flathub has no catalogue yet: fetch it once.
    if apps_of(&table).is_empty() {
        let _ = flatpak(&["update", "--user", "--appstream", "--noninteractive"]);
        table = found()?;
    }
    Ok(apps_of(&table).into_iter().take(40).collect())
}

/// Where Flathub's catalogue keeps an application's icon, if it has one.
pub fn app_icon(id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let p = PathBuf::from(home).join(".local/share/flatpak/appstream/flathub").join(std::env::consts::ARCH).join("active/icons/64x64").join(format!("{id}.png"));
    p.exists().then_some(p)
}

fn checked(id: &str) -> Result<()> {
    if !crate::tree::flatpak_id_ok(id) {
        anyhow::bail!("'{id}' is not an application's id");
    }
    Ok(())
}

pub fn install_app(id: &str) -> Result<()> {
    checked(id)?;
    flatpak(&["install", "--user", "--noninteractive", "flathub", id]).map(drop)
}

/// Remove an application. What it saved in the person's home stays, for if they come back to it.
pub fn remove_app(id: &str) -> Result<()> {
    checked(id)?;
    flatpak(&["uninstall", "--user", "--noninteractive", id]).map(drop)
}

pub fn open_app(id: &str) {
    if checked(id).is_ok() {
        spawn("flatpak", &["run", id]);
    }
}

/// What to do to bring what is installed, what the configuration names, and what TorqOS last knew it named
/// into line. `managed` is what tells a person's own removal from a machine that has yet to install.
#[derive(Debug, Default, PartialEq)]
pub struct AppPlan {
    /// Installed here and not yet remembered (the Store's, or just installed): into the configuration.
    pub keep: Vec<String>,
    /// Remembered, known here, and gone: the person removed it. Out of the configuration.
    pub forget: Vec<String>,
    /// Named by the configuration and never here (a rebuilt machine): install it.
    pub install: Vec<String>,
    /// Taken out of the configuration (an undo): remove it.
    pub remove: Vec<String>,
}

pub fn plan_apps(declared: &BTreeSet<String>, managed: &BTreeSet<String>, installed: &BTreeSet<String>) -> AppPlan {
    let list = |f: &dyn Fn(&String) -> bool, from: &BTreeSet<String>| from.iter().filter(|x| f(x)).cloned().collect();
    AppPlan {
        keep: list(&|x| !declared.contains(x) && !managed.contains(x), installed),
        forget: list(&|x| managed.contains(x) && !installed.contains(x), declared),
        install: list(&|x| !managed.contains(x) && !installed.contains(x), declared),
        remove: list(&|x| !declared.contains(x) && installed.contains(x), managed),
    }
}

fn managed_file() -> Result<PathBuf> {
    use anyhow::Context as _;
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".local/state/torq/flatpaks-managed.json"))
}

fn managed_apps() -> BTreeSet<String> {
    managed_file().ok().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn set_managed(apps: &BTreeSet<String>) -> Result<()> {
    let p = managed_file()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_vec(apps)?)?;
    Ok(())
}

/// A value list of this person's in the configuration in effect: `users users <me> <leaf>`.
fn my_list(s: &mut crate::session::Session<'_>, leaf: &str) -> Result<BTreeSet<String>> {
    let me = crate::me()?;
    let want = ["users".to_string(), "users".to_string(), me, leaf.to_string()];
    Ok(settings_of(&s.exec("show configuration | display set")?).into_iter().filter(|(p, _)| *p == want).map(|(_, v)| v).collect())
}

/// Change this person's own settings, on a candidate of its own (never the draft), and commit them.
fn commit_mine(lines: &[String], comment: &str) -> Result<()> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Draft)?;
    for l in lines {
        s.exec(l)?;
    }
    s.exec(&format!("commit comment {}", line(&[comment]))).map(drop)
}

fn app_lines(verb: &str, ids: &[String]) -> Result<Vec<String>> {
    let me = crate::me()?;
    Ok(ids.iter().map(|id| line(&[verb, "users", "users", &me, "flatpaks", id])).collect())
}

/// Remove these applications: those that would not go, still to be removed.
fn removing(ids: &[String]) -> Vec<String> {
    ids.iter().filter(|id| remove_app(id).is_err()).cloned().collect()
}

/// What TorqOS knows of afterwards: what the configuration names and is here, and `also`.
fn managed_after(declared: &BTreeSet<String>, installed: &BTreeSet<String>, also: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    declared.intersection(installed).cloned().chain(also).collect()
}

/// Remember what this person has installed and forget what they removed, in their own configuration: no
/// password, a line in the history. Whether anything changed.
pub fn sync_apps() -> Result<bool> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Read)?;
    let (declared, managed) = (my_list(&mut s, "flatpaks")?, managed_apps());
    let installed: BTreeSet<String> = installed_apps()?.into_iter().map(|a| a.id).collect();
    let plan = plan_apps(&declared, &managed, &installed);
    let changes = !plan.keep.is_empty() || !plan.forget.is_empty();
    if changes {
        let mut lines = app_lines("set", &plan.keep)?;
        lines.extend(app_lines("delete", &plan.forget)?);
        let words = |verb: &str, ids: &[String]| if ids.is_empty() { None } else { Some(format!("{verb} {}", ids.join(", "))) };
        let comment = [words("Apps: keep", &plan.keep), words("Apps: forget", &plan.forget)].into_iter().flatten().collect::<Vec<_>>().join("; ");
        commit_mine(&lines, &comment)?;
    }
    // What the configuration no longer names (an undo) goes now: left here, the next look would keep it again.
    let unremoved = removing(&plan.remove);
    set_managed(&managed_after(&declared, &installed, plan.keep.iter().cloned().chain(unremoved)))?;
    Ok(changes || !plan.remove.is_empty())
}

/// Bring what is installed in line with the configuration: a rebuilt machine's apps come back, an undone install
/// goes. Applications the person installed and has yet to be remembered are left for `sync_apps`.
pub fn reconcile_apps() -> Result<()> {
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    let mut s = open_session(&cli, &real, None, At::Read)?;
    let (declared, managed) = (my_list(&mut s, "flatpaks")?, managed_apps());
    let installed: BTreeSet<String> = installed_apps()?.into_iter().map(|a| a.id).collect();
    let plan = plan_apps(&declared, &managed, &installed);
    // A person's removal stays known, for `sync_apps` to take out of the configuration: dropped here, it would be
    // taken for a machine that has yet to install, and come back.
    let mut also = plan.forget.clone();
    also.extend(plan.install.iter().filter(|id| install_app(id).is_ok()).cloned());
    also.extend(removing(&plan.remove));
    set_managed(&managed_after(&declared, &installed, also))
}

// ── Tools: command-line software, a person's own ─────────────────────────────────────────────────

/// A package `show packages` found: its name, version and description.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub version: String,
    pub summary: String,
}

/// `show packages`' lines.
pub fn tools_of(shown: &str) -> Vec<Tool> {
    shown
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let (name, version) = (w.next()?, w.next()?);
            crate::tree::package_name_ok(name).then(|| Tool { name: name.to_string(), version: version.to_string(), summary: w.collect::<Vec<_>>().join(" ") })
        })
        .collect()
}

/// Add a tool for this person: their own configuration, no password, a line in the history.
pub fn add_tool(name: &str) -> Result<()> {
    let me = crate::me()?;
    commit_mine(&[line(&["set", "users", "users", &me, "packages", name])], &format!("Tools: add {name}"))
}

pub fn remove_tool(name: &str) -> Result<()> {
    let me = crate::me()?;
    commit_mine(&[line(&["delete", "users", "users", &me, "packages", name])], &format!("Tools: remove {name}"))
}

/// Who is at the machine: the name in `users users <me>`.
pub fn who() -> Result<String> {
    crate::me()
}

#[cfg(test)]
mod app_tests {
    use super::*;

    fn set(s: &[&str]) -> BTreeSet<String> {
        s.iter().map(|x| x.to_string()).collect()
    }
    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn what_to_do_about_apps() {
        // Installed by hand or just now: remembered, not removed.
        let p = plan_apps(&set(&[]), &set(&[]), &set(&["a.b.store"]));
        assert_eq!((p.keep, p.remove), (v(&["a.b.store"]), v(&[])));
        // A rebuilt machine: named, never here. Installed, not forgotten.
        let p = plan_apps(&set(&["a.b.c"]), &set(&[]), &set(&[]));
        assert_eq!((p.install, p.forget), (v(&["a.b.c"]), v(&[])));
        // The person removed it here: forgotten, not installed again.
        let p = plan_apps(&set(&["a.b.c"]), &set(&["a.b.c"]), &set(&[]));
        assert_eq!((p.forget, p.install), (v(&["a.b.c"]), v(&[])));
        // Undone in the history: out of the configuration, and it goes. Not remembered again.
        let p = plan_apps(&set(&[]), &set(&["a.b.c"]), &set(&["a.b.c"]));
        assert_eq!((p.remove, p.keep), (v(&["a.b.c"]), v(&[])));
        // All in step: nothing.
        assert_eq!(plan_apps(&set(&["a.b.c"]), &set(&["a.b.c"]), &set(&["a.b.c"])), AppPlan::default());
    }

    #[test]
    fn what_flatpak_and_show_packages_print() {
        let apps = apps_of("org.mozilla.firefox\tFirefox\tA browser\nnot an id\tX\tY\n");
        assert_eq!(apps, vec![App { id: "org.mozilla.firefox".into(), name: "Firefox".into(), summary: "A browser".into() }]);
        let tools = tools_of("  htop   3.4.1   An interactive process viewer\n");
        assert_eq!(tools[0], Tool { name: "htop".into(), version: "3.4.1".into(), summary: "An interactive process viewer".into() });
    }
}

/// When a process started, in clock ticks since boot: the 22nd field of its `/proc/<pid>/stat`, counted after
/// the command's name, which may itself hold spaces and parentheses (so after its last `)`).
fn start_time(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

fn started(pid: i32) -> Option<u64> {
    start_time(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// polkit's start, if this person's approval dialog (COSMIC's agent, `cosmic-osd`) is older than the polkit now
/// running: it registered with one that has gone (a switch that changes polkit restarts it), does not register
/// again by itself, and no approval can be asked of the person until it starts again. Read afresh each time, from
/// the processes themselves, so an applet that starts after polkit was replaced still sees it.
///
/// polkit is the `polkitd` that polkit's own account runs, never a process of that name another account could
/// start (the agent's, say, to have the person's dialog ended every few seconds: the review of this fix). The
/// dialogs are the person's in each of their sessions: with two logins, each applet starts both again, once.
pub fn approval_agent_outlived() -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let polkit_uid = polkit_account()?;
    let polkit = std::fs::read_dir("/proc").ok()?.flatten().find_map(|e| {
        let pid: i32 = e.file_name().to_str()?.parse().ok()?;
        (e.metadata().ok()?.uid() == polkit_uid).then_some(())?;
        (std::fs::read_to_string(e.path().join("comm")).ok()?.trim() == "polkitd").then_some(pid)
    })?;
    let polkit = started(polkit)?;
    let agent = my_parts().into_iter().filter(|(_, c)| c == "cosmic-osd").filter_map(|(pid, _)| started(pid)).min()?;
    (agent < polkit).then_some(polkit)
}

/// The account polkit runs as on NixOS (`polkituser`), from /etc/passwd.
fn polkit_account() -> Option<u32> {
    passwd_uid(&std::fs::read_to_string("/etc/passwd").ok()?, "polkituser")
}

fn passwd_uid(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next()? == name).then_some(())?;
        f.nth(1)?.parse().ok()
    })
}

#[cfg(test)]
#[test]
fn polkits_account_is_read_from_passwd() {
    let passwd = "root:x:0:0::/root:/bin/sh\npolkituser:x:28:989:PolKit daemon:/var/empty:/bin/nologin\npolkituser2:x:5:5::/:/bin/sh\n";
    assert_eq!(passwd_uid(passwd, "polkituser"), Some(28));
    assert_eq!(passwd_uid(passwd, "polkit"), None);
    assert_eq!(passwd_uid("polkituser:x:notanumber:1::/:/bin/sh", "polkituser"), None);
}

#[cfg(test)]
#[test]
fn a_start_time_is_read_after_the_commands_name() {
    let stat = |comm: &str| format!("4242 ({comm}) S 1 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 1 0 987654 12345678 300");
    assert_eq!(start_time(&stat("polkitd")), Some(987654));
    // A name with a space and a parenthesis of its own does not shift the fields.
    assert_eq!(start_time(&stat("a) b (c")), Some(987654));
    assert_eq!(start_time("4242 (cut"), None);
    assert_eq!(start_time("4242 (short) S 1 2"), None);
}

/// Start COSMIC's approval dialog again, so that it registers with polkit.
pub fn restart_approval_agent() {
    restart_parts(&["cosmic-osd"]);
}

// ── Backing the configuration up to a repository ─────────────────────────────────────────────────

/// What the Backup tab offers once a repository is connected.
pub const BACKUP: &[Action] = &[run("Load what waits on the repository into your draft", "request system pull"), run("Clear the repository's alarm, having looked into it", "request system remote clear")];

/// A GitHub repository's owner and name, from its SSH address.
pub fn github_repo(url: &str) -> Option<(String, String)> {
    let path = url.strip_prefix("git@github.com:").or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let (owner, repo) = path.trim_end_matches('/').trim_end_matches(".git").split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_string(), repo.to_string()))
}

/// This machine's name.
pub fn host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string()
}

/// Open a web page in the person's browser. Only https: what is opened is built here.
pub fn open_url(url: &str) {
    if url.starts_with("https://") {
        spawn("xdg-open", &[url]);
    }
}

#[cfg(test)]
#[test]
fn a_github_address_names_a_repository() {
    let got = |o: &str, r: &str| Some((o.to_string(), r.to_string()));
    assert_eq!(github_repo("git@github.com:netstacks/torqos-laptop.git"), got("netstacks", "torqos-laptop"));
    assert_eq!(github_repo("ssh://git@github.com/netstacks/x"), got("netstacks", "x"));
    assert_eq!(github_repo("git@gitlab.com:a/b.git"), None);
    assert_eq!(github_repo("git@github.com:a/b/c.git"), None);
}

// ── Signing in to GitHub, for setting a repository up ────────────────────────────────────────────

/// TorqOS's registration with GitHub: a public identifier, for the device flow (no secret goes with it).
const GITHUB_CLIENT: &str = "Ov23liVjvg3exXm64Wxz";

/// A person's GitHub sign-in: held by the window for the setup, written nowhere, and never printed.
#[derive(Clone)]
pub struct GitHubToken(String);

impl std::fmt::Debug for GitHubToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHubToken(hidden)")
    }
}

/// What a person types at GitHub to sign this machine in.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    device_code: String,
    interval: u64,
    expires_in: u64,
}

/// A value for a curl configuration file: quoted, so a value cannot add an option of its own.
fn curl_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n"))
}

/// The configuration curl reads on standard input: the token and the body stay out of the process list.
fn curl_config(token: Option<&GitHubToken>, body: Option<&str>) -> String {
    let mut c = String::from("header = \"Accept: application/json\"\nheader = \"User-Agent: TorqOS\"\n");
    if let Some(t) = token {
        c.push_str(&format!("header = {}\n", curl_quoted(&format!("Authorization: Bearer {}", t.0))));
    }
    if let Some(b) = body {
        c.push_str(&format!("data = {}\n", curl_quoted(b)));
    }
    c
}

/// A request to GitHub, over HTTPS only: its status and JSON.
fn github(url: &str, token: Option<&GitHubToken>, body: Option<&str>) -> Result<(u16, serde_json::Value)> {
    use anyhow::Context as _;
    use std::io::Write as _;
    let mut child = std::process::Command::new("curl")
        .args(["--disable", "--silent", "--show-error", "--proto", "=https", "--max-time", "30", "--write-out", "\n%{http_code}", "--config", "-", url])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("running curl")?;
    child.stdin.take().context("curl's input")?.write_all(curl_config(token, body).as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!("could not reach GitHub: {}", history::one_line(&String::from_utf8_lossy(&out.stderr), 200));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (json, code) = text.rsplit_once('\n').context("GitHub's answer")?;
    Ok((code.trim().parse().unwrap_or(0), serde_json::from_str(json).unwrap_or(serde_json::Value::Null)))
}

/// Ask GitHub for a code the person approves in their browser.
pub fn github_start() -> Result<DeviceCode> {
    let body = format!("client_id={GITHUB_CLIENT}&scope=repo");
    let (_, v) = github("https://github.com/login/device/code", None, Some(&body))?;
    device_code_of(&v)
}

fn device_code_of(v: &serde_json::Value) -> Result<DeviceCode> {
    use anyhow::Context as _;
    let text = |k: &str| v[k].as_str().map(String::from);
    Ok(DeviceCode {
        user_code: history::one_line(&text("user_code").with_context(|| format!("GitHub did not give a code: {}", history::one_line(&v.to_string(), 200)))?, 20),
        verification_uri: text("verification_uri").filter(|u| u.starts_with("https://github.com/")).context("GitHub gave no page to open")?,
        device_code: text("device_code").context("GitHub gave no device code")?,
        interval: v["interval"].as_u64().unwrap_or(5),
        expires_in: v["expires_in"].as_u64().unwrap_or(900),
    })
}

/// Where a sign-in has got to.
#[derive(Debug, PartialEq)]
enum Poll {
    Signed(String),
    Wait,
    Slower,
    Stopped(String),
}

fn poll_of(v: &serde_json::Value) -> Poll {
    if let Some(t) = v["access_token"].as_str() {
        return Poll::Signed(t.to_string());
    }
    match v["error"].as_str() {
        Some("authorization_pending") => Poll::Wait,
        Some("slow_down") => Poll::Slower,
        Some("expired_token") => Poll::Stopped("the code expired: start again".into()),
        Some("access_denied") => Poll::Stopped("the sign-in was declined at GitHub".into()),
        other => Poll::Stopped(format!("GitHub said: {}", history::one_line(other.unwrap_or("nothing we understand"), 120))),
    }
}

/// Wait while the person approves the code: their sign-in, and their account's name.
pub fn github_wait(code: &DeviceCode) -> Result<(GitHubToken, String)> {
    let body = format!("client_id={GITHUB_CLIENT}&device_code={}&grant_type=urn:ietf:params:oauth:grant-type:device_code", code.device_code);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(1);
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(interval));
        // A dropped connection is not the end of a sign-in: ask again.
        let Ok((status, v)) = github("https://github.com/login/oauth/access_token", None, Some(&body)) else { continue };
        if status >= 500 || v.is_null() {
            continue;
        }
        match poll_of(&v) {
            Poll::Signed(t) => {
                let token = GitHubToken(t);
                let (_, me) = github("https://api.github.com/user", Some(&token), None)?;
                let login = me["login"].as_str().map(|l| history::one_line(l, 60)).unwrap_or_default();
                return Ok((token, login));
            }
            Poll::Wait => {}
            Poll::Slower => interval += 5,
            Poll::Stopped(why) => anyhow::bail!("{why}"),
        }
    }
    anyhow::bail!("the code expired: start again")
}

/// Create a private repository, or find it if the person already has it: its SSH address.
pub fn github_create_repo(token: &GitHubToken, login: &str, name: &str) -> Result<String> {
    use anyhow::Context as _;
    if !repo_name_ok(name) {
        anyhow::bail!("a repository's name is letters, numbers, dots, dashes and underscores");
    }
    let body = serde_json::json!({ "name": name, "private": true, "description": format!("TorqOS configuration of {}", host()) }).to_string();
    let (code, v) = github("https://api.github.com/user/repos", Some(token), Some(&body))?;
    if code == 201 {
        return v["ssh_url"].as_str().map(String::from).context("GitHub gave no address");
    }
    if code == 422 {
        let (found, v) = github(&format!("https://api.github.com/repos/{login}/{name}"), Some(token), None)?;
        if found == 200 {
            // A configuration names the machine's people and software: never into a public repository.
            if v["private"].as_bool() != Some(true) {
                anyhow::bail!("{login}/{name} already exists and is public: choose another name");
            }
            return v["ssh_url"].as_str().map(String::from).context("GitHub gave no address");
        }
    }
    anyhow::bail!("GitHub would not create it: {}", history::one_line(v["message"].as_str().unwrap_or("no reason given"), 200))
}

/// Give a repository this machine's key, able to write to it and to nothing else.
pub fn github_add_key(token: &GitHubToken, owner: &str, repo: &str, key: &str) -> Result<()> {
    let body = serde_json::json!({ "title": format!("TorqOS {}", host()), "key": key, "read_only": false }).to_string();
    let (code, v) = github(&format!("https://api.github.com/repos/{owner}/{repo}/keys"), Some(token), Some(&body))?;
    if code == 201 {
        return Ok(());
    }
    // 422 is this key already on this repository, or on another (a key serves one repository): only the first is done.
    if code == 422 {
        let (listed, keys) = github(&format!("https://api.github.com/repos/{owner}/{repo}/keys?per_page=100"), Some(token), None)?;
        if listed == 200 && keys.as_array().is_some_and(|k| k.iter().any(|e| e["key"].as_str().is_some_and(|have| same_key(have, key)))) {
            return Ok(());
        }
    }
    anyhow::bail!("GitHub would not take the key: {}", history::one_line(&github_refusal(&v), 200))
}

/// GitHub's reason: its message, and the first detail under it (where "key is already in use" is said).
fn github_refusal(v: &serde_json::Value) -> String {
    let message = v["message"].as_str().unwrap_or("no reason given");
    match v["errors"][0]["message"].as_str() {
        Some(detail) => format!("{message}: {detail}"),
        None => message.to_string(),
    }
}

/// Whether two public keys are one: their type and body, whatever comment follows.
fn same_key(a: &str, b: &str) -> bool {
    let body = |k: &str| k.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    !body(a).is_empty() && body(a) == body(b)
}

/// A name GitHub would give a repository, and nothing that could change the address it goes into.
fn repo_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && name.len() <= 100 && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

#[cfg(test)]
mod github_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sign_in_as_github_answers_it() {
        assert_eq!(poll_of(&json!({"access_token": "t"})), Poll::Signed("t".into()));
        assert_eq!(poll_of(&json!({"error": "authorization_pending"})), Poll::Wait);
        assert_eq!(poll_of(&json!({"error": "slow_down"})), Poll::Slower);
        assert!(matches!(poll_of(&json!({"error": "access_denied"})), Poll::Stopped(_)));
        assert!(matches!(poll_of(&json!({})), Poll::Stopped(_)));
        let c = device_code_of(&json!({"user_code": "AB-12", "verification_uri": "https://github.com/login/device", "device_code": "d", "interval": 5, "expires_in": 900})).unwrap();
        assert_eq!(c.user_code, "AB-12");
        assert!(device_code_of(&json!({"user_code": "AB", "verification_uri": "https://evil.example/", "device_code": "d"})).is_err(), "only GitHub's own page");
    }

    #[test]
    fn what_goes_to_curl_cannot_become_an_option() {
        assert_eq!(curl_quoted("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
        let c = curl_config(Some(&GitHubToken("sec\"\nurl = \"http://x".into())), None);
        assert_eq!(c.lines().filter(|l| l.starts_with("url")).count(), 0, "{c}");
    }

    #[test]
    fn a_key_is_its_body_and_a_name_stays_a_name() {
        assert!(same_key("ssh-ed25519 AAAA torq-broker@x", "ssh-ed25519 AAAA"));
        assert!(!same_key("ssh-ed25519 AAAA", "ssh-ed25519 BBBB"));
        assert!(!same_key("", ""));
        assert!(repo_name_ok("torqos-torqlaptop"));
        for bad in ["", "..", "a/b", "a?x=1", "a b"] {
            assert!(!repo_name_ok(bad), "{bad}");
        }
        assert_eq!(github_refusal(&json!({"message": "Validation Failed", "errors": [{"message": "key is already in use"}]})), "Validation Failed: key is already in use");
    }

    #[test]
    fn a_sign_in_is_never_printed() {
        assert_eq!(format!("{:?}", GitHubToken("secret".into())), "GitHubToken(hidden)");
    }
}

#[cfg(test)]
mod apps_remembered {
    use super::*;

    fn set(a: &[&str]) -> BTreeSet<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn an_undo_that_could_not_finish_is_not_kept_again() {
        let (none, x) = (set(&[]), set(&["x"]));
        let p = plan_apps(&none, &x, &x);
        assert_eq!(p.remove, ["x"]);
        let after = managed_after(&none, &x, p.remove.clone());
        assert!(plan_apps(&none, &after, &x).keep.is_empty(), "the undo must hold");
    }

    #[test]
    fn a_removal_seen_at_login_is_not_installed_back() {
        let (none, x) = (set(&[]), set(&["x"]));
        let p = plan_apps(&x, &x, &none);
        assert_eq!(p.forget, ["x"]);
        let after = managed_after(&x, &none, p.forget.clone());
        let next = plan_apps(&x, &after, &none);
        assert!(next.install.is_empty(), "the person removed it");
        assert_eq!(next.forget, ["x"]);
    }

    #[test]
    fn only_ones_own_unfinished_commit_is_not_waiting() {
        let p = |seq, caller: &str, actor: &str| history::Entry {
            seq,
            ts: String::new(),
            kind: "proposal".into(),
            tier: "system".into(),
            actor: actor.into(),
            approved_by: None,
            tainted: false,
            reason: String::new(),
            data: serde_json::json!({ "caller": caller }),
        };
        let e = [p(1, "me", "user"), p(2, "sam", "user"), p(3, "torq-agent", "agent")];
        let seqs: Vec<u64> = waiting_on_me(&e, "me").iter().map(|x| x.seq).collect();
        assert_eq!(seqs, [2, 3]);
    }
}

// ── A person's desktop, kept in their configuration ──────────────────────────────────────────────

pub use crate::desktop::{Saving, Status as DesktopStatus, Synced};

/// One sync at a time for a person, whichever program starts it (the applet, TorqOS Settings, torqcli):
/// two at once would save the same change twice.
fn one_desktop_sync() -> Result<std::fs::File> {
    let p = crate::desktop::Paths::of_home()?.state.with_extension("lock");
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&p)?;
    f.lock()?;
    Ok(f)
}

/// Bring this person's desktop and configuration together (crate::desktop::sync): what the configuration
/// has newer goes on the desktop; their changes are saved if saving is automatic, or `save_now`.
pub fn sync_desktop(save_now: bool) -> Result<Synced> {
    let _one = one_desktop_sync()?;
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    crate::desktop::sync(&real, &crate::candidate::Repo::new(&cli.repo), &crate::me()?, &crate::desktop::Paths::of_home()?, save_now)
}

/// `show system desktop`'s facts.
pub fn desktop_status() -> Result<DesktopStatus> {
    crate::desktop::status(&crate::candidate::Repo::new(&cli().repo), &crate::me()?, &crate::desktop::Paths::of_home()?)
}

/// `request system desktop saving automatic|manual`.
pub fn desktop_saving(how: Saving) -> Result<String> {
    let _one = one_desktop_sync()?;
    let cli = cli();
    let real = crate::shell::Real::new(&cli);
    crate::desktop::set_saving(&real, &crate::candidate::Repo::new(&cli.repo), &crate::me()?, how)
}

/// The words for what is not saved yet: "panel, theme".
pub fn desktop_words(keys: &[String]) -> String {
    crate::desktop::what(keys.iter().cloned())
}

/// What the next save takes out that an earlier TorqOS kept, in a sentence (None: nothing).
pub fn desktop_taking_out(keys: &[String]) -> Option<String> {
    crate::desktop::taking_out(keys)
}
