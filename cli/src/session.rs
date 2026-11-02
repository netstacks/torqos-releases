//! A torqcli session (ADR-016, docs/CLI-DESIGN.md): operational and configuration mode, and the
//! commands of each in J syntax. Everything that reaches the broker or evaluates Nix goes
//! through `Machine`, so the whole command layer is testable without either.

use crate::candidate::{self, Candidate, Repo};
use crate::syntax::{self, CommitOptions, Display, FileAction, LoadFrom, LoadHow, Op};
use crate::whole::Data;
use crate::tree::Schema;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// What the session needs from the machine: evaluation and the broker.
pub trait Machine {
    /// `commit check`: does the configuration at commit `base`, with these tree files changed
    /// (None: deleted), evaluate?
    fn check(&self, base: &str, files: &[(String, Option<String>)]) -> Result<()>;
    /// Propose `diff` against `base` and apply it (with `confirm` minutes: commit confirmed). The
    /// broker's tier must be `tier`. Returns the applied ledger entry's number.
    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64>;
    /// A commit confirmed waiting, if there is one.
    fn pending_confirm(&self) -> Result<Option<Waiting>>;
    /// Keep the waiting commit confirmed.
    fn confirm(&self) -> Result<u64>;
    /// Ask a person (a system-tier commit, a delete of everything, a repair, a revert).
    fn ask(&self, question: &str) -> Answer;
    /// A person edits `text` (the config file `path`) in their editor; None: they left it unchanged.
    fn edit(&self, path: &str, text: &str) -> Result<Option<String>>;
    /// Why `text` is not Nix as written (Nix's own words, with the place in `path`), if it is not:
    /// parsed only, never evaluated.
    fn nix_syntax(&self, path: &str, text: &str) -> Result<Option<String>>;
    /// What a person pastes or types at the terminal, up to Ctrl-D (`load … terminal`), at most
    /// `max` bytes.
    fn read_terminal(&self, max: usize) -> Result<String>;
    /// `show system alarms`: what is not as the ledger says (and a commit confirmed waiting); with
    /// `detail`, the broker's facts behind it (commits, store paths).
    fn alarms(&self, detail: bool, fit: Option<usize>) -> Result<String>;
    /// `show packages <term>…`.
    /// `show packages <words>`: nixpkgs searched, closest first; `installed` (the configuration's)
    /// marked.
    fn search(&self, terms: &[String], installed: &std::collections::BTreeSet<String>, fit: Option<usize>) -> Result<String>;
    /// `request system repair`: return the machine to its last ledgered configuration.
    fn repair(&self) -> Result<String>;
    /// `request system password [<user>]`: a person types the new password; it is set and kept
    /// outside the config repo, ledgered without the hash.
    fn set_password(&self, user: &str) -> Result<String>;
    /// SSH as the configuration at `base` has it, and as it will with `files` changed — evaluated, so
    /// every way of turning it on counts (`services openssh`, its `sshd` alias, a config file, the old
    /// list).
    fn ssh(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(Ssh, Ssh)>;
    /// Turning SSH on: who it admits, and the keys that will be able to log in as them — read from
    /// the configuration at `base` with `files` changed, as it will be. One line per key (and a
    /// warning line above a key with options); none: nobody could log in.
    fn ssh_keys(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(String, Vec<String>)>;
    /// `show system ledger <count>`: the ledger's last entries, as a person reads them.
    fn ledger(&self, count: usize, fit: Option<usize>) -> Result<String>;
    /// The running machine: its versions, uptime, interfaces and disks (facts.rs).
    fn facts(&self) -> Result<crate::facts::Facts>;
    /// `request system revert <change>`: undo that one change (a new change forward), asked as a
    /// commit is (`me`: who is asking; their own folder needs no question).
    fn revert(&self, seq: u64, me: &str) -> Result<String>;
    /// The configurations so far, newest first (at most `count`): the numbers of `rollback <n>` and
    /// `show system commit`. From the ledger (ADR-016): its entries that produced a configuration.
    fn configurations(&self, repo: &Repo, count: usize) -> Result<History>;
    /// `request system update [all]`: the `flake.lock` that `nix flake update` (of `torq`, or with
    /// `all` every input) makes from the configuration at commit `base`.
    fn update_lock(&self, base: &str, all: bool) -> Result<String>;
    /// `set system software torq source`: the `flake.lock` that locking `torq` (with `all`, every
    /// input) makes, from the configuration at commit `base` with its flake.nix as `flake_nix`.
    fn lock_with(&self, base: &str, flake_nix: &str, all: bool) -> Result<String>;
    /// Whether commit `rev` of TorqOS's releases is signed by the release key (release.rs): what
    /// git says of its signature, or why it is refused.
    fn check_release(&self, rev: &str) -> Result<String>;
}


/// The configurations (newest first), and where the ledger says the config repo is — by the
/// broker's own count (`ledgered_commit`), which repairs and interrupted changes are part of.
#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub configurations: Vec<Configuration>,
    pub at: Option<String>,
}

/// One configuration in the history: the commit, and the ledger entry that made it.
#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub id: String,
    pub date: String,
    /// Who: the person who approved it, else the actor (`broker` for an automatic rollback).
    pub who: String,
    pub what: String,
    /// The ledger entry (None: read from git, without one).
    pub seq: Option<u64>,
}

/// A person's answer to a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// No terminal to ask at (a script, a pipe).
    NobodyToAsk,
}

impl Answer {
    /// What an answer means for the command that asked, `not_done` its outcome declined
    /// ("committed"): yes, go on (None); no, a choice, said, not a failure ("Not committed."); no one
    /// to ask, refused.
    pub fn go_on(self, not_done: &str) -> Result<Option<String>> {
        match self {
            Answer::Yes => Ok(None),
            Answer::No => Ok(Some(format!("Not {not_done}.\n"))),
            Answer::NobodyToAsk => bail!("not {not_done}: this needs a person to approve it at a terminal"),
        }
    }
}

/// Where the notice of changes not yet committed is said (Session::pending_note).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// Into configuration mode, with changes from before.
    Entering,
    /// Out of configuration mode (the shell's candidate: kept).
    Leaving,
    /// Out of torqcli (kept).
    Quitting,
    /// At the end of `-c` (a candidate of its own: gone).
    Discarded,
}

/// SSH in an evaluated configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Ssh {
    pub on: bool,
    /// Passwords as well as keys (`services openssh settings PasswordAuthentication`).
    pub passwords: bool,
}

/// A commit confirmed waiting for its author.
#[derive(Debug, Clone, PartialEq)]
pub struct Waiting {
    /// Its change (the applied ledger entry).
    pub seq: u64,
    pub deadline: String,
    /// Who made it: the one who may confirm it.
    pub by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Operational,
    Configuration,
}

pub struct Session<'a> {
    pub schema: Schema,
    /// Every NixOS setting of the system (nixos.rs): the tree's paths.
    pub tree: crate::nixos::Tree,
    pub repo: Repo,
    /// Where the candidate is kept between sessions; None: a private one (`torqcli -c`), discarded
    /// at the end.
    pub saved: Option<PathBuf>,
    pub mode: Mode,
    /// Where configuration mode is (`edit system`).
    edit: Vec<String>,
    candidate: Option<Candidate>,
    machine: &'a dyn Machine,
    /// Who is at the shell (their login name): their own folder needs no question.
    me: String,
    /// How wide the terminal the output goes to is, read for each command (it may be resized);
    /// None: not a terminal (a script, a test). The shell sets it.
    pub screen: fn() -> Option<usize>,
    /// The width to lay this command's output out to: the screen's, unless a filter reads it
    /// (`| grep` and `| wc -l` count lines, so a line stays one line).
    fit: Option<usize>,
    /// Set by `exit` in operational mode (or `quit`).
    pub done: bool,
    /// Whether the last command's output may be shown a screenful at a time (not `| no-more`).
    pub page: bool,
}

/// Split a command line into words as a shell does: whitespace-separated, "double" or 'single'
/// quotes for a word with spaces, `|` always its own word.
pub fn words(line: &str) -> Result<Vec<String>> {
    let (mut out, mut cur, mut any) = (Vec::new(), String::new(), false);
    // The quote open, if one is: `"` or `'`.
    let mut open: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        let quoted = open == Some('"');
        match c {
            // Inside single quotes everything is itself, up to the next one.
            _ if open == Some('\'') => {
                if c == '\'' {
                    open = None;
                } else {
                    cur.push(c);
                }
            }
            '"' => {
                open = if quoted { None } else { Some('"') };
                any = true;
            }
            '\'' if !quoted => {
                open = Some('\'');
                any = true;
            }
            // As in a shell's double quotes: `\"` and `\\` are escapes, any other `\` is itself (so a
            // `grep "\d+"` pattern arrives whole).
            '\\' if quoted => match chars.next().context("a line cannot end in \\")? {
                c @ ('"' | '\\') => cur.push(c),
                c => {
                    cur.push('\\');
                    cur.push(c);
                }
            },
            c if c.is_whitespace() && !quoted => {
                if any {
                    word(&mut out, std::mem::take(&mut cur))?;
                    any = false;
                }
            }
            '|' if !quoted => {
                if any {
                    word(&mut out, std::mem::take(&mut cur))?;
                    any = false;
                }
                out.push("|".into());
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    match open {
        Some('\'') => bail!("missing closing ' (an apostrophe in text: put the text in \"double quotes\")"),
        Some(q) => bail!("missing closing {q}"),
        None => {}
    }
    if any {
        word(&mut out, cur)?;
    }
    Ok(out)
}

/// A word from the text (not a pipe): a quoted `"|"` alone would read as one, so it is refused.
fn word(out: &mut Vec<String>, w: String) -> Result<()> {
    if w == "|" {
        bail!("\"|\" alone cannot be a word: it would read as a pipe");
    }
    out.push(w);
    Ok(())
}

impl<'a> Session<'a> {
    pub fn new(schema: Schema, tree: crate::nixos::Tree, repo: Repo, saved: Option<PathBuf>, machine: &'a dyn Machine, me: &str) -> Self {
        Session { schema, tree, repo, saved, mode: Mode::Operational, edit: Vec::new(), candidate: None, machine, me: me.into(), screen: || None, fit: None, done: false, page: true }
    }

    /// The whole tree over both kinds of files (whole.rs).
    fn whole(&self) -> crate::whole::Whole<'_> {
        crate::whole::Whole { schema: &self.schema, tree: &self.tree }
    }

    fn candidate(&mut self) -> Result<&mut Candidate> {
        if self.candidate.is_none() {
            let c = match &self.saved {
                Some(p) => Candidate::load(p, &self.schema)?,
                None => None,
            };
            self.candidate = Some(match c {
                Some(c) => c,
                None => Candidate::from_repo(&self.repo, &self.schema, "HEAD")?,
            });
        }
        Ok(self.candidate.as_mut().unwrap())
    }

    fn save(&self) -> Result<()> {
        if let (Some(p), Some(c)) = (&self.saved, &self.candidate) {
            c.save(p)?;
        }
        Ok(())
    }

    /// Throw the candidate away: the next edit starts from the active configuration.
    fn discard(&mut self) -> Result<()> {
        self.candidate = None;
        if let Some(p) = &self.saved {
            Candidate::discard(p)?;
        }
        Ok(())
    }

    /// Run one command line; returns what to print.
    pub fn exec(&mut self, line: &str) -> Result<String> {
        self.exec_words(&words(line)?)
    }

    /// Whether `line` would commit (not `commit check`): after failed lines the shell asks first, so
    /// a commit never takes part of a paste unseen.
    pub fn commits(&self, line: &str) -> bool {
        // (Read exactly as it would run — abbreviated, or from the top — so no form slips past.)
        fn commits(op: &Op) -> bool {
            match op {
                Op::Commit(o) => !o.check,
                Op::Top { then: Some(op) } => commits(op),
                _ => false,
            }
        }
        words(line).ok().and_then(|w| self.read(&w).ok()).is_some_and(|l| commits(&l.op))
    }

    /// Whether `line` shows all the candidate changes (`show | compare`, from the top): the person
    /// has looked.
    pub fn reviews(&self, line: &str) -> bool {
        let whole = |op: &Op, at_top: bool| at_top && matches!(op, Op::Compare { .. });
        words(line).ok().and_then(|w| self.read(&w).ok()).is_some_and(|l| match &l.op {
            Op::Top { then: Some(op) } => whole(op, true),
            op => whole(op, self.edit.is_empty()),
        })
    }

    /// A line's words as the session runs them: read by the grammar (grammar.rs: abbreviations become
    /// their words; a line that is not a command says why).
    fn read(&self, w: &[String]) -> Result<crate::grammar::Line> {
        crate::grammar::read_line(self.mode, w)
    }

    /// Run one command, already split into words (`torqcli <command>`: as the calling shell split
    /// them; a word that is exactly `|` is a pipe).
    pub fn exec_words(&mut self, w: &[String]) -> Result<String> {
        // A comment (`## software: …` in a saved configuration) is not a command.
        if w.first().is_some_and(|x| x.starts_with('#')) {
            return Ok(String::new());
        }
        if w.is_empty() {
            return Ok(String::new());
        }
        let line = self.read(w)?;
        self.page = line.page;
        self.fit = if line.filters.is_empty() { crate::ui::fit((self.screen)()) } else { None };
        crate::filter::apply(&line.filters, self.run(line.op)?)
    }

    /// How this command's sentences and tables are laid out (ui.rs): to fit the window, unless a
    /// filter reads them. Plain: the shell cleans, then paints, what is printed.
    fn ui(&self) -> crate::ui::Ui {
        crate::ui::Ui::plain_at(self.fit)
    }

    /// A sentence a command answers with, as its own line(s): wrapped to the window (ADR-025, one
    /// output path). Data — a configuration, a comparison, a table — is not a sentence.
    fn said(&self, text: &str) -> String {
        format!("{}\n", self.ui().line("", text))
    }

    /// A sentence marked `! ` (paint.rs shows it as a warning).
    fn noted(&self, text: &str) -> String {
        format!("{}\n", self.ui().line("! ", text))
    }

    /// The rules between settings (whole::software_allowed) are checked on the whole configuration,
    /// as `commit` checks them — never line by line, where their order would decide. After a change
    /// that breaks one, the person hears it at once; `commit` refuses until it holds.
    fn rules_note(&self, d: &Data) -> String {
        match crate::whole::software_allowed(d) {
            Ok(()) => String::new(),
            Err(e) => self.noted(&format!("{e:#} — `commit` refuses until then")),
        }
    }

    /// Whether `words`, from where configuration mode is, name a place (somewhere to go), not a setting.
    /// Where configuration mode is (`edit system`): its words, for the prompt.
    pub fn edit_path(&self) -> &[String] {
        &self.edit
    }

    /// `words` from where configuration mode is.
    fn here(&self, words: &[String]) -> Vec<String> {
        self.edit.iter().chain(words).cloned().collect()
    }

    fn run(&mut self, op: Op) -> Result<String> {
        match op {
            Op::Configure => self.configure(),
            Op::ShowActive { path, display } => {
                // The commit named is the one shown: read at that id, not at HEAD again.
                let head = self.repo.head()?;
                let active = self.configuration_of(&head)?;
                let mut out = self.view(&active, &path, display, &format!("commit {head}"))?;
                if path.is_empty() {
                    out.push_str(&self.footer(&head, None)?);
                }
                Ok(out)
            }
            Op::ShowCommits { .. } | Op::ShowRollback { .. } | Op::ShowAlarms { .. } | Op::ShowServices | Op::ShowPackages { .. } | Op::ShowLedger { .. } | Op::ShowVersion | Op::ShowUptime | Op::ShowInterfaces { .. } | Op::ShowStorage | Op::ShowUsers => {
                self.show_operational(op)
            }
            Op::Request(r) => self.request(r),
            Op::File { action, path } => self.file(action, path),
            Op::Save { file } => self.save_to(&file),
            Op::Load { how, from } => self.load_from(how, &from),
            Op::Exit if self.mode == Mode::Operational => {
                self.done = true;
                // The interactive candidate is kept for the next `configure`: say so on the way out.
                Ok(if self.saved.is_some() && self.uncommitted().unwrap_or(false) { self.pending_note(Pending::Quitting) } else { String::new() })
            }
            Op::Exit if !self.edit.is_empty() => {
                self.up();
                Ok(String::new())
            }
            Op::Exit | Op::End => self.leave(),
            Op::Help { topic } => Ok(self.help(&topic)),
            Op::Up { n } => {
                if self.edit.is_empty() {
                    bail!("already at the top of the configuration");
                }
                for _ in 0..n {
                    if self.edit.is_empty() {
                        break;
                    }
                    self.up();
                }
                Ok(String::new())
            }
            Op::Top { then: None } => {
                self.edit.clear();
                Ok(String::new())
            }
            // A command from the top, as on a network device: back where you were after it — unless
            // it is itself a move (`top edit system`).
            Op::Top { then: Some(op) } => {
                // A move: `edit`, `up`, `exit` (a `top` inside `top` is not a line: grammar.rs).
                let moved = matches!(*op, Op::Edit { .. } | Op::Up { .. } | Op::Top { .. } | Op::Exit | Op::End);
                let was = std::mem::take(&mut self.edit);
                let r = self.run(*op);
                // Back where it was — unless it moved, or left configuration mode (`commit and-quit`).
                if (!moved || r.is_err()) && self.mode == Mode::Configuration {
                    self.edit = was;
                }
                r
            }
            Op::Edit { words } => {
                let full = self.here(&words);
                self.candidate()?;
                if !self.whole().is_place(&full) {
                    // Not a place: no such setting at all (said with a hint), or a setting.
                    if let Err(e) = self.tree.locate(&full) {
                        if crate::whole::hand_words(&full).is_none() {
                            return Err(e);
                        }
                    }
                    bail!("{}: a setting, not a place to go: `set {} <value>`", full.join(" "), words.iter().map(|w| syntax::quote(w)).collect::<Vec<_>>().join(" "));
                }
                self.edit = full;
                Ok(String::new())
            }
            Op::Set { words } => {
                let full = self.here(&words);
                let mut d = self.candidate_data()?;
                let was = d.source.clone();
                self.whole().set(&mut d, &full)?;
                let note = self.rules_note(&d);
                if d.source != was {
                    // A new source is locked now, in the candidate: what `commit` installs is shown first.
                    let shown = self.lock_source(d.source.as_deref().unwrap_or_default(), false)?;
                    self.put_data(d)?;
                    self.save()?;
                    return Ok(format!("{shown}{note}"));
                }
                self.put_data(d)?;
                self.save()?;
                Ok(note)
            }
            Op::Delete { words } => {
                let full = self.here(&words);
                // Nothing after `delete`: everything here (the whole configuration, at the top) — asked
                // first, as on a network device; a script cannot answer, so it is refused there.
                if words.is_empty() {
                    let here = if full.is_empty() { "the whole configuration".to_string() } else { format!("everything under {}", full.join(" ")) };
                    if let Some(stop) = self.machine.ask(&format!("Delete {here}? ")).go_on("deleted")? {
                        return Ok(stop);
                    }
                }
                let mut d = self.candidate_data()?;
                let done = self.whole().delete(&mut d, &full)?;
                if done == crate::whole::Deleted::NotFound {
                    return Ok(self.noted(&format!("nothing deleted: {} is not set", if full.is_empty() { "anything".to_string() } else { full.join(" ") })));
                }
                let note = self.rules_note(&d);
                self.put_data(d)?;
                self.save()?;
                Ok(note)
            }
            Op::ShowCandidate { path, display } => {
                let d = self.candidate_data()?;
                let c = self.candidate()?;
                let (base, lock) = (c.base.clone(), c.lock.clone());
                let here = self.here(&path);
                let mut out = self.view(&d, &here, display, &format!("the candidate, from commit {base}"))?;
                if here.is_empty() {
                    out.push_str(&self.footer(&base, lock.as_deref())?);
                }
                Ok(out)
            }
            Op::Compare { with, display } => self.compare(with, display),
            Op::Rollback { n, path } if path.is_empty() => self.rollback(n),
            Op::Rollback { n, path } => self.rollback_part(n, &path),
            Op::Commit(o) => {
                let quit = o.and_quit;
                let out = self.commit(o)?;
                if !quit {
                    return Ok(out);
                }
                self.mode = Mode::Operational;
                self.edit.clear();
                Ok(format!("{out}{}", self.said("Exiting configuration mode")))
            }
            Op::Run(op) => {
                let was = self.mode;
                self.mode = Mode::Operational;
                let r = self.run(*op);
                self.mode = was;
                self.done = false;
                r
            }
        }
    }

    /// Up one level: out of a list entry (its name and the list), else one word.
    fn up(&mut self) {
        // One level: a set of named entries is a level of its own (`… virtualHosts example.com` →
        // `… virtualHosts`), as a network device's `interfaces` is above `ge-0/0/0`.
        self.edit.pop();
    }

    /// The one notice of changes not yet committed, and what becomes of them where it is said.
    pub fn pending_note(&self, at: Pending) -> String {
        let then = match at {
            Pending::Entering => "`show | compare` lists them, `rollback 0` discards them",
            Pending::Leaving => "they are kept (`configure` returns to them, `rollback 0` discards them)",
            Pending::Quitting => "they are kept for your next `configure`",
            Pending::Discarded => "they are discarded: these commands work on a candidate of their own (end with `commit` to keep them)",
        };
        self.said(&format!("The candidate has changes not yet committed: {then}."))
    }

    /// Out of configuration mode, saying if the candidate holds changes.
    fn leave(&mut self) -> Result<String> {
        // Leave first: a candidate that cannot be read must not keep anyone in here.
        self.mode = Mode::Operational;
        self.edit.clear();
        let changed = self.uncommitted().unwrap_or(true);
        Ok(match (changed, self.saved.is_some()) {
            (false, _) => String::new(),
            (true, true) => self.pending_note(Pending::Leaving),
            // (A script's: said once, when it ends.)
            (true, false) => String::new(),
        })
    }

    /// What a whole configuration at `rev` does not show in its tree, as comments: the software it
    /// runs (`lock`: the candidate's, if it changes it) and the files outside the tree that also set
    /// things (Nix). So `show configuration` names everything that makes the device, and stays a
    /// file `load` reads (`#` lines are comments).
    /// The software the configuration at `rev` runs, as its lock names it.
    fn software(&self, rev: &str) -> Result<Option<String>> {
        Ok(self.repo.file_at(rev, "flake.lock")?.and_then(|b| software_of(&String::from_utf8_lossy(&b))))
    }

    fn footer(&self, rev: &str, lock: Option<&str>) -> Result<String> {
        let mut out = String::new();
        let lock = match lock {
            Some(l) => Some(l.to_string()),
            None => self.repo.file_at(rev, "flake.lock")?.map(|b| String::from_utf8_lossy(&b).into_owned()),
        };
        if let Some(shown) = lock.as_deref().and_then(software_of) {
            out.push_str(&format!("## software: {shown}\n"));
        }
        // A config file attached in the tree is part of the configuration (shown as `<place> config <file>`).
        let attached = self.configuration_of(rev)?.attached;
        let outside: Vec<String> = self
            .repo
            .paths(rev)?
            .into_iter()
            // (flake.nix is TorqOS's own: its software is the line above.)
            .filter(|p| p != "flake.lock" && p != "flake.nix" && p != crate::attach::FILE && !self.schema.describes(p) && crate::settings::top_of(p).is_none())
            .filter(|p| !crate::attach::files(&attached).any(|f| f == p)).map(|p| crate::history::one_line(&p, 60)).collect();
        if !outside.is_empty() {
            out.push_str(&format!("## also set outside this configuration, in Nix: {}\n", outside.join(", ")));
        }
        Ok(out)
    }

    /// A configuration, or the part below `path`, as `display` asks. `origin`: where it comes from
    /// (`| display detail`).
    fn view(&self, d: &Data, path: &[String], display: Option<Display>, origin: &str) -> Result<String> {
        let w = self.whole();
        match display {
            // Nothing set at a place: said, not an empty answer.
            None => w.show(d, path).map(|out| if out.is_empty() && !path.is_empty() { format!("Nothing is set at {}.\n", path.join(" ")) } else { out }),
            Some(Display::Set) => w.display_set(d, path),
            Some(Display::Detail) => {
                let mut out = w.show(d, path)?;
                out.push_str(&format!("## {origin}\n"));
                // Each file behind what is shown, with the settings it holds.
                let mut by_file: std::collections::BTreeMap<String, Vec<String>> = Default::default();
                for (words, file) in w.files(d) {
                    if words.starts_with(path) {
                        by_file.entry(file).or_default().push(words.join(" "));
                    }
                }
                for (file, held) in by_file {
                    out.push_str(&format!("## {file}: {}\n", held.join(", ")));
                }
                Ok(out)
            }
            Some(Display::Diff) => bail!("| display diff is for a comparison"),
        }
    }

    fn configure(&mut self) -> Result<String> {
        self.mode = Mode::Configuration;
        let head = self.repo.head()?;
        let mut out = self.said("Entering configuration mode");
        let base = self.candidate()?.base.clone();
        if base == head && self.pending()? > 0 {
            out.push_str(&self.pending_note(Pending::Entering));
        }
        let c = self.candidate()?;
        if c.base != head {
            let (was, now) = (short(&c.base).to_string(), short(&head).to_string());
            out.push_str(&self.said(&format!(
                "The configuration has been changed since your candidate was started ({was} → {now}): \
                 `show | compare` is against the old one, and `commit` will be refused; \
                 `rollback 0` starts over from the current configuration."
            )));
        }
        Ok(out)
    }

    fn show_operational(&mut self, op: Op) -> Result<String> {
        match op {
            Op::ShowCommits { display } => {
                let mut out = String::new();
                let h = self.machine.configurations(&self.repo, 50)?;
                let all = &h.configurations;
                // Each configuration: its rollback number (0 is in effect), its change number (the
                // ledger's, as `request system revert` takes it), when, who, what.
                use crate::ui::{Col, Table};
                let mut t = Table::new(&[Col::Left("Rollback"), Col::Left("Change"), Col::Left("When"), Col::Left("By"), Col::Text("What")]);
                for (n, c) in all.iter().enumerate() {
                    t.row(vec![n.to_string(), c.seq.map_or("-".to_string(), |s| format!("#{s}")), c.date.clone(), c.who.clone(), c.what.clone()]);
                    if display == Some(Display::Detail) {
                        t.line(format!("    commit {}", crate::history::one_line(&c.id, 64)));
                    }
                }
                out.push_str(&t.render(&self.ui()));
                // The ledger says where the config repo is (by the broker's count); if it is elsewhere, 0 —
                // what is active — is not the newest line above.
                let head = self.repo.head()?;
                if h.at.as_ref().is_some_and(|at| *at != head) {
                    out.push_str(&format!("## the config repo is at {}, not where the ledger says: `show system alarms`\n", short(&head)));
                }
                Ok(out)
            }
            Op::ShowRollback { n, compare, display } => {
                let n_id = self.commit_at(n)?;
                let n_data = self.configuration_of(&n_id)?;
                match compare {
                    None => Ok(self.view(&n_data, &[], display, &format!("commit {n_id} (rollback {n})"))? + &self.footer(&n_id, None)?),
                    // As on a network device, n is the base: what going from configuration n to
                    // configuration m changes (`+`: what m has).
                    Some(m) => {
                        let m_id = self.commit_at(m)?;
                        match display {
                            Some(Display::Diff) => {
                                let (old, new) = (self.repo.all_files(&n_id)?, self.repo.all_files(&m_id)?);
                                commits_diff(&old, &new)
                            }
                            _ => {
                                let m_data = self.configuration_of(&m_id)?;
                                let out = self.whole().compare(&n_data, &m_data);
                                Ok(if out.is_empty() { self.said(&format!("No differences: configurations {n} and {m} have the same settings.")) } else { out })
                            }
                        }
                    }
                }
            }
            Op::ShowAlarms { display } => self.machine.alarms(display == Some(Display::Detail), self.fit),
            Op::ShowServices => {
                // What the configuration in effect turns on: NixOS's services (`services <name> enable`,
                // `virtualisation <name> enable`), and what the old list still turns on.
                let head = self.repo.head()?;
                let active = self.configuration_of(&head)?;
                let on_in_tree = self.whole().entries(&active).into_iter().filter_map(|(w, v)| match (w.as_slice(), &v) {
                    ([top, name, enable], crate::tree::Val::One(t)) if (top == "services" || top == "virtualisation") && enable == "enable" && t == "true" => Some(vec![top.clone(), name.clone()]),
                    _ => None,
                });
                let old: Vec<&str> = match active.config.files.get("services.json") {
                    Some(crate::tree::Data::List(l)) => l.iter().filter_map(|n| crate::whole::OLD_SERVICES.iter().find(|(old, _)| old == n)).map(|(_, new)| *new).collect(),
                    _ => vec![],
                };
                let from_old: Vec<Vec<String>> = old.iter().map(|new| new.split(' ').take(2).map(String::from).collect()).collect();
                let on: std::collections::BTreeSet<Vec<String>> = on_in_tree.chain(from_old.iter().cloned()).collect();
                if on.is_empty() {
                    return Ok(self.said("No services are turned on in the configuration: `set services <name> enable true` turns one on (`help <word>` finds it)."));
                }
                let fit = self.ui().width();
                let mut rows = Vec::new();
                for place in &on {
                    let mut help = self.tree.resolve(&[place.clone(), vec!["enable".into()]].concat()).map(|r| r.help.to_string()).unwrap_or_default();
                    // SSH: how TorqOS has it, as the configuration evaluates (modules/services.nix).
                    if place[1] == "openssh" {
                        let (_, ssh) = self.machine.ssh(&head, &[])?;
                        help = if ssh.passwords { "SSH: passwords and keys, repeated wrong guesses blocked; root never logs in".into() } else { "SSH: keys only; root never logs in".into() };
                    }
                    rows.push((place.join(" "), help));
                }
                let mut out = format!("Services the configuration turns on:\n{}", crate::complete::table(&rows, fit));
                if !old.is_empty() {
                    let moves: Vec<String> = old.iter().map(|new| format!("`set {new}`")).collect();
                    let how = format!("Some come from the old list of services (services.json). To move them: {}, `delete torq services enabled`, then commit.", moves.join(", "));
                    out.push_str(&self.noted(&how));
                }
                Ok(out)
            }
            Op::ShowPackages { terms } => {
                // What the configuration in effect installs, for everyone or for anyone.
                let head = self.repo.head()?;
                let active = self.configuration_of(&head)?;
                let installed = self
                    .whole()
                    .entries(&active)
                    .into_iter()
                    .filter(|(w, _)| w.as_slice() == ["environment", "systemPackages"] || (w.len() == 4 && w[0] == "users" && w[3] == "packages"))
                    .flat_map(|(_, v)| match v {
                        crate::tree::Val::One(p) => vec![p],
                        crate::tree::Val::Many(ps) => ps,
                    })
                    .collect();
                self.machine.search(&terms, &installed, self.fit)
            }
            Op::ShowVersion => {
                let head = self.repo.head()?;
                let software = self.software(&head)?.unwrap_or_else(|| "(its flake.lock names no inputs)".into());
                Ok(crate::facts::version(&self.machine.facts()?, &software, &self.ui()))
            }
            Op::ShowUptime => {
                let all = self.machine.configurations(&self.repo, 1)?.configurations;
                let last = all.first().map(|c| {
                    let n = c.seq.map(|s| format!("#{s}, ")).unwrap_or_default();
                    format!("{n}{}, by {}: {}", c.date, c.who, crate::history::one_line(&c.what, usize::MAX))
                });
                Ok(crate::facts::uptime(&self.machine.facts()?, last.as_deref(), &self.ui()))
            }
            Op::ShowInterfaces { name } => crate::facts::interfaces(&self.machine.facts()?, name.as_deref()),
            Op::ShowStorage => Ok(crate::facts::storage(&self.machine.facts()?, &self.ui())),
            Op::ShowUsers => Ok(crate::facts::users(&self.machine.facts()?, &self.ui())),
            Op::ShowLedger { count } => self.machine.ledger(count.unwrap_or(20), self.fit),
            other => bail!("not an operational show: {other:?}"),
        }
    }

    /// The commit n changes back (`rollback <n>`'s numbering: 0 is the current one).
    /// 0 is what is active (HEAD), whatever the ledger says; the others are the ledger's.
    fn commit_at(&self, n: usize) -> Result<String> {
        if n == 0 {
            return self.repo.head();
        }
        let all = self.machine.configurations(&self.repo, n + 1)?.configurations;
        let c = all.get(n).with_context(|| format!("there is no configuration {n}: there are {} (0 to {}; `show system commit` lists them)", all.len(), all.len().saturating_sub(1)))?;
        if !self.repo.has_commit(&c.id)? {
            bail!("configuration {n} (commit {}) is no longer in the config repo", short(&c.id));
        }
        Ok(c.id.clone())
    }

    /// The configuration at commit `id`.
    fn configuration_of(&self, id: &str) -> Result<Data> {
        let files = self.repo.tree_files(&self.schema, id)?;
        data_of(&self.schema, &files)
    }

    /// What `?` dry-runs a line against for `<[Enter]>` (complete.rs): the candidate in configuration
    /// mode, the active configuration in operational mode; nothing, if it cannot be read.
    pub fn help_data(&mut self) -> Data {
        match self.mode {
            Mode::Configuration => self.candidate_data().ok(),
            Mode::Operational => self.repo.head().ok().and_then(|h| self.configuration_of(&h).ok()),
        }
        .unwrap_or_default()
    }

    /// The candidate's tree data.
    fn candidate_data(&mut self) -> Result<Data> {
        let base_source = self.base_source()?;
        let c = self.candidate()?;
        let source = c.source.clone().or(base_source);
        Ok(Data { config: c.config.clone(), settings: c.settings.clone(), attached: c.attached.clone(), source })
    }

    /// The software's source at the candidate's base (its flake.nix).
    fn base_source(&mut self) -> Result<Option<String>> {
        let base = self.candidate()?.base.clone();
        Ok(self.repo.file_at(&base, "flake.nix")?.and_then(|b| crate::software::source_of(&String::from_utf8_lossy(&b))))
    }

    /// Put the tree data back into the candidate (not saved yet).
    fn put_data(&mut self, d: Data) -> Result<()> {
        let c = self.candidate()?;
        (c.config, c.settings, c.attached, c.source) = (d.config, d.settings, d.attached, d.source);
        Ok(())
    }

    /// `help`: how to use torqcli; `help <command>`: that command and what may follow it; `help
    /// <words>`: the commands and settings whose names or descriptions have every word.
    fn help(&self, topic: &[String]) -> String {
        let fit = self.ui().width();
        let mode_name = |m: Mode| if m == Mode::Operational { "operational mode" } else { "configuration mode" };
        if topic.is_empty() {
            let rows: Vec<(String, String)> = crate::grammar::first_words(self.mode).into_iter().map(|(w, h)| (w.to_string(), h.to_string())).collect();
            let how = if self.mode == Mode::Operational { "`configure` edits the configuration" } else { "`commit` makes the candidate the machine's configuration; `exit` leaves" };
            return format!(
                "{}\nCommands:\n{}\n{}\n",
                crate::ui::hang("", &format!("torqcli, TorqOS's command line, in {} ({how}).", mode_name(self.mode)), Some(fit), fit),
                crate::complete::table(&rows, fit),
                crate::ui::hang("", "Anywhere: `?` lists what may come next, with what each is; Tab completes a word; a word may be shortened while it is the only one it could be (`sh sys al`). `help <command>` explains a command; `help <word>` searches the commands and settings.", Some(fit), fit),
            );
        }
        if let Some((line, help, next)) = crate::grammar::describe(self.mode, topic) {
            let rows: Vec<(String, String)> = next.into_iter().map(|(w, h)| (w, h.to_string())).collect();
            let after = if rows.is_empty() { String::new() } else { format!("What may follow:\n{}", crate::complete::table(&rows, fit)) };
            return format!("{}\n{after}", crate::ui::hang("", &format!("{line}: {help}."), Some(fit), fit));
        }
        // A search: every word found in a command's or a setting's name or its help (found_in); the
        // closest first.
        let terms: Vec<String> = topic.iter().map(|t| t.to_lowercase()).collect();
        let score = |name: &str, help: &str| -> Option<usize> { terms.iter().map(|t| found_in(t, name, help)).try_fold(0, |sum, s| s.map(|s| sum + s)) };
        let mut out = String::new();
        let other = if self.mode == Mode::Operational { Mode::Configuration } else { Mode::Operational };
        for m in [self.mode, other] {
            let mut found: Vec<(usize, String, String)> = crate::grammar::commands(m).into_iter().filter_map(|(l, h)| score(&l, h).map(|s| (s, l, h.to_string()))).collect();
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1)));
            let rows: Vec<(String, String)> = found.into_iter().map(|(_, l, h)| (l, h)).collect();
            if !rows.is_empty() {
                let heading = if m == self.mode { "Commands:".to_string() } else { format!("Commands in {}:", mode_name(m)) };
                out.push_str(&format!("{}{heading}\n{}", if out.is_empty() { "" } else { "\n" }, crate::complete::table(&rows, fit)));
            }
        }
        // What other systems call it, where it is here (`ssh`: services openssh), before the places.
        let usual: Vec<(String, String)> = crate::vocabulary::concepts_of(&terms).into_iter().map(|c| (c.place.to_string(), c.how.to_string())).collect();
        if !usual.is_empty() {
            out.push_str(&format!("{}On TorqOS:\n{}", if out.is_empty() { "" } else { "\n" }, crate::complete::table(&usual, fit)));
        }
        // Places first (a service, a section: what someone searching for "ssh" is looking for), then
        // single settings. A place is said by what turning it on does, else by what it holds.
        let mut places: Vec<(usize, String, String)> = Vec::new();
        let mut settings: Vec<(usize, String, String)> = Vec::new();
        let mut stack: Vec<(Vec<String>, &crate::nixos::Node)> = vec![(vec![], &self.tree.root)];
        while let Some((path, n)) = stack.pop() {
            if path.last().is_some_and(|w| w.starts_with('_')) || (!path.is_empty() && self.tree.fenced(&path)) {
                continue;
            }
            let name = path.join(" ");
            if let Some((_, help)) = &n.setting {
                // Closest first: the better each word is found, then the shorter path.
                if let Some(s) = score(&name, help) {
                    settings.push((usize::MAX - s, name.clone(), help.clone()));
                }
            } else if !path.is_empty() && !n.children.is_empty() && !path.last().is_some_and(|w| w.starts_with('<') || w == "*") {
                let help = n.children.get("enable").and_then(|e| e.setting.as_ref()).map(|(_, h)| h.clone()).unwrap_or_else(|| crate::nixos::section_help(n));
                if let Some(s) = score(&name, "") {
                    places.push((usize::MAX - s, name, help));
                }
            }
            for (w, c) in n.children.iter().rev() {
                stack.push(([path.clone(), vec![w.clone()]].concat(), c));
            }
        }
        let closest = |a: &(usize, String, String), b: &(usize, String, String)| a.0.cmp(&b.0).then(a.1.len().cmp(&b.1.len())).then(a.1.cmp(&b.1));
        places.sort_by(closest);
        settings.sort_by(closest);
        if !places.is_empty() {
            const PLACES: usize = 10;
            let rows: Vec<(String, String)> = places.iter().take(PLACES).map(|(_, p, h)| (p.clone(), h.clone())).collect();
            out.push_str(&format!("{}Places (edit <place>; ? there lists its settings):\n{}", if out.is_empty() { "" } else { "\n" }, crate::complete::table(&rows, fit)));
        }
        if !settings.is_empty() {
            const SHOWN: usize = 25;
            let rows: Vec<(String, String)> = settings.iter().take(SHOWN).map(|(_, p, h)| (p.clone(), h.clone())).collect();
            out.push_str(&format!("{}Settings (set <setting> <value>):\n{}", if out.is_empty() { "" } else { "\n" }, crate::complete::table(&rows, fit)));
            if settings.len() > SHOWN {
                out.push_str(&format!("  … and {} more: more words narrow it (help {} <word>)\n", settings.len() - SHOWN, topic.join(" ")));
            }
        }
        if out.is_empty() {
            return format!("{}\n", crate::ui::hang("", &format!("Nothing has \"{}\" in its name or description. `help` lists the commands; `?` lists what may come next.", topic.join(" ")), Some(fit), fit));
        }
        out
    }

    fn request(&mut self, r: syntax::Request) -> Result<String> {
        match r {
            syntax::Request::Repair => self.machine.repair(),
            syntax::Request::Password { user } => self.machine.set_password(user.as_deref().unwrap_or(&self.me.clone())),
            syntax::Request::Revert { change } => self.machine.revert(change, &self.me),
            syntax::Request::Update { all } => self.update(all),
            syntax::Request::SoftwareRollback { n } => self.software_rollback(n),
        }
    }

    /// `request system update [all]`: the newest TorqOS (with `all`, every input) into the candidate.
    fn update(&mut self, all: bool) -> Result<String> {
        let base = self.candidate()?.base.clone();
        let source = self.base_source()?;
        // With a new source in the candidate, the update is of that source (its flake.nix as it will be).
        let new = match self.candidate()?.source.clone() {
            Some(url) if Some(&url) != source.as_ref() => {
                let flake = self.repo.file_at(&base, "flake.nix")?.context("this machine has no flake.nix")?;
                let flake = crate::software::with_source(&String::from_utf8(flake).context("flake.nix is not text")?, &url)?;
                self.machine.lock_with(&base, &flake, all)?
            }
            // An image's own TorqOS has no newer version: the first update moves the machine to the
            // releases (ADR-028), as `set system software torq source` would.
            _ if source.as_deref().is_some_and(crate::software::image_own) => {
                let releases = crate::software::RELEASES;
                let mut d = self.candidate_data()?;
                d.source = Some(releases.into());
                let shown = self.lock_source(releases, all)?;
                self.put_data(d)?;
                self.save()?;
                let moved = self.said(&format!("This machine runs the TorqOS it was installed with: the update moves it to TorqOS's releases ({releases})."));
                return Ok(format!("{moved}{shown}"));
            }
            _ => self.machine.update_lock(&base, all)?,
        };
        let signed = self.release_signed(&new)?;
        Ok(format!("{}{signed}", self.load_software(new, "This machine has the newest software: nothing to load.")?))
    }

    /// `set system software torq source <url>`: lock that source (the newest it has; with `all`,
    /// every input too), as `request system update` locks the current one, into the candidate.
    fn lock_source(&mut self, url: &str, all: bool) -> Result<String> {
        let base = self.candidate()?.base.clone();
        let flake = self.repo.file_at(&base, "flake.nix")?.context("this machine has no flake.nix")?;
        let flake = crate::software::with_source(&String::from_utf8(flake).context("flake.nix is not text")?, url)?;
        let lock = self.machine.lock_with(&base, &flake, all)?;
        let signed = self.release_signed(&lock)?;
        let base_lock = self.repo.file_at(&base, "flake.lock")?;
        let shown = software_compare(base_lock.as_deref(), &Some(lock.clone()));
        self.candidate()?.lock = Some(lock);
        Ok(format!("{shown}{signed}{}", self.said("The new source is locked into the candidate: `commit` installs it.")))
    }

    /// If `lock` takes TorqOS from the releases, the release key's word on that commit (ADR-028): one
    /// it did not sign is refused, before anything is loaded.
    fn release_signed(&self, lock: &str) -> Result<String> {
        let Some(rev) = crate::software::release_commit(lock) else { return Ok(String::new()) };
        self.machine.check_release(&rev)?;
        Ok(self.said(&format!("TorqOS release {}: signed by the TorqOS release key.", short(&rev))))
    }

    /// `request system software rollback <n>`: commit n's software into the candidate.
    fn software_rollback(&mut self, n: usize) -> Result<String> {
        if self.candidate()?.source.clone().is_some_and(|s| Some(s) != self.base_source().ok().flatten()) {
            bail!("the candidate changes where TorqOS comes from: an earlier configuration's software is of another source (`rollback 0` starts over)");
        }
        let id = self.commit_at(n)?;
        // A lock belongs to the flake.nix it was committed with: configuration n's, if TorqOS came from
        // elsewhere then, is not this one's (audit of 2026-10-31).
        let then = self.repo.file_at(&id, "flake.nix")?.and_then(|b| crate::software::source_of(&String::from_utf8_lossy(&b)));
        let now = self.base_source()?;
        if then != now {
            bail!(
                "configuration {n}'s software came from {}, not {}: a lock belongs to its source (set the source back first)",
                then.as_deref().unwrap_or("(a flake.nix TorqOS does not read)"),
                now.as_deref().unwrap_or("?")
            );
        }
        let lock = self.repo.file_at(&id, "flake.lock")?.with_context(|| format!("configuration {n} has no flake.lock"))?;
        let lock = String::from_utf8(lock).with_context(|| format!("configuration {n}'s flake.lock is not text"))?;
        self.load_software(lock, &format!("Configuration {n} has this machine's software already: nothing to load."))
    }

    /// Put `lock` in the candidate as its software and say what it changes (or `same`, if nothing).
    fn load_software(&mut self, lock: String, same: &str) -> Result<String> {
        if serde_json::from_str::<serde_json::Value>(&lock).map_or(true, |v| !v.is_object()) {
            bail!("that flake.lock is not JSON; nothing loaded");
        }
        let head = self.repo.head()?;
        let base = self.candidate()?.base.clone();
        let base_lock = self.repo.file_at(&base, "flake.lock")?;
        let shown = software_compare(base_lock.as_deref(), &Some(lock.clone()));
        let c = self.candidate()?;
        let had = c.lock.is_some();
        c.lock = (!shown.is_empty()).then_some(lock);
        self.save()?;
        let mut out = match (shown.is_empty(), had) {
            (true, false) => self.said(same),
            (true, true) => format!("{}{}", self.said(&same.replace(": nothing to load", "")), self.said("The software loaded into the candidate before is dropped.")),
            (false, _) => format!("{shown}{}", self.said("Loaded into the candidate: `commit` (in configuration mode) installs it.")),
        };
        if base != head {
            out.push_str(&self.said(&format!(
                "The configuration has changed since your candidate was started ({} → {}): `commit` will be \
                 refused; `rollback 0` starts over (without this software), then load it again.",
                short(&base),
                short(&head)
            )));
        }
        Ok(out)
    }

    /// What the candidate changes from its base (`| display diff`: as the raw diff of the files).
    fn compare(&mut self, with: Option<usize>, display: Option<Display>) -> Result<String> {
        let a = match with {
            Some(n) => {
                let id = self.commit_at(n)?;
                self.against_at(&id)?
            }
            None => self.against()?,
        };
        if display == Some(Display::Diff) {
            let scratch = tempfile::tempdir()?;
            return candidate::unified_diff(scratch.path(), &a.changes(&self.schema)?);
        }
        // As on a network device: from `[edit networking]`, what changes there and below it.
        let out = sections_under(&a.compare(&self.whole())?, &self.edit);
        if out.is_empty() {
            let here = if self.edit.is_empty() { String::new() } else { " here".into() };
            let what = with.map_or("the configuration in effect".to_string(), |n| format!("configuration {n} (rollback {n})"));
            return Ok(self.said(&format!("No changes{here}: the candidate is {what}.")));
        }
        Ok(out)
    }

    fn rollback(&mut self, n: usize) -> Result<String> {
        let id = self.commit_at(n)?;
        // A configuration, not software: what the candidate had loaded (`request system update`) stays —
        // except for `rollback 0`, which starts over.
        // The software's source is the base's after it (a configuration, not software), so a lock made for
        // another source is dropped with it: a lock always belongs to the flake.nix it is committed with.
        let other_source = self.candidate()?.source.clone().is_some_and(|s| Some(s) != self.base_source().ok().flatten());
        let kept = if n == 0 || other_source { None } else { self.candidate()?.lock.clone() };
        let before = self.pending()?;
        let mut c = Candidate::from_repo(&self.repo, &self.schema, &id)?;
        c.lock = kept.clone();
        self.candidate = Some(c);
        self.save()?;
        let after = self.pending()?;
        let mut out = match (n, after) {
            (0, _) if before == 0 => "No changes to discard: the candidate is the configuration in effect.".to_string(),
            (0, _) => format!("Discarded {}: the candidate is the configuration in effect.", changes(before)),
            (_, 0) => format!("Loaded configuration {n}: it is the same as the configuration in effect."),
            (_, a) => format!("Loaded configuration {n}: {} from the configuration in effect (`show | compare` lists them; `commit` makes it the machine's).", changes(a)),
        };
        if kept.is_some() {
            out.push_str(" The software loaded into the candidate is kept.");
        }
        Ok(self.said(&out))
    }

    /// `rollback <n> <path>`: the place `path` (from where configuration mode is) put back as
    /// configuration n has it, the rest of the candidate as it is (n = 0: one change undone).
    fn rollback_part(&mut self, n: usize, path: &[String]) -> Result<String> {
        let here = self.here(path);
        let then = self.configuration_of(&self.commit_at(n)?)?;
        let mut d = self.candidate_data()?;
        let before = self.pending()?;
        let whole = self.whole();
        let at = self.tree.locate(&here).map(|l| l.0).unwrap_or_else(|_| here.clone());
        // Out, as the candidate has it; in, as configuration n has it.
        whole.delete(&mut d, &here)?;
        for (words, value) in whole.entries(&then).into_iter().filter(|(w, _)| w.starts_with(&at)) {
            let value: Vec<String> = match value {
                crate::tree::Val::One(v) => vec![v],
                crate::tree::Val::Many(v) => [vec!["[".to_string()], v, vec!["]".to_string()]].concat(),
            };
            whole.set(&mut d, &[words, value].concat())?;
        }
        self.put_data(d)?;
        self.save()?;
        let after = self.pending()?;
        let which = if n == 0 { "the configuration in effect".to_string() } else { format!("configuration {n}") };
        let what = match before.saturating_sub(after) {
            0 if before == after => "it already was".to_string(),
            k => format!("{} undone", changes(k)),
        };
        Ok(self.said(&format!("Put back {} as {which} has it: {what}.", at.join(" "))))
    }

    /// The configuration's Nix files (as `file list` lists them: the candidate's in configuration mode).
    pub fn nix_files(&mut self) -> Vec<String> {
        self.file(FileAction::List, None).map(|out| out.lines().map(|l| l.trim().to_string()).filter(|l| l.ends_with(".nix")).collect()).unwrap_or_default()
    }

    /// How many changes the candidate holds (Against::count).
    fn pending(&mut self) -> Result<usize> {
        let a = self.against()?;
        a.count(&self.whole())
    }

    /// Whether the candidate differs from its base.
    pub fn uncommitted(&mut self) -> Result<bool> {
        if self.candidate.is_none() && self.saved.as_ref().is_none_or(|p| !p.exists()) {
            return Ok(false);
        }
        let a = self.against()?;
        Ok(!a.changes(&self.schema)?.is_empty())
    }

    /// The candidate and what it is based on, read once.
    fn against(&mut self) -> Result<Against> {
        let base = self.candidate()?.base.clone();
        self.against_at(&base)
    }

    /// The candidate and the configuration at commit `base` (its own base, or another one:
    /// `show | compare rollback <n>`), read once.
    fn against_at(&mut self, base: &str) -> Result<Against> {
        let data = self.candidate_data()?;
        let base = base.to_string();
        let (own, new_lock, mut nix) = {
            let c = self.candidate()?;
            (c.base.clone(), c.lock.clone(), c.nix.clone())
        };
        // Against another configuration, the candidate is its edits on top of its own base: its
        // config files and software as the base has them, where it has not changed them.
        let new_lock = if base == own { new_lock } else { new_lock.or(self.repo.file_at(&own, "flake.lock")?.map(|l| String::from_utf8_lossy(&l).into_owned())) };
        if base != own {
            let paths: std::collections::BTreeSet<String> = self.repo.paths(&own)?.into_iter().chain(self.repo.paths(&base)?).filter(|p| p.ends_with(".nix") && p != "flake.nix").collect();
            for p in paths {
                if !nix.contains_key(&p) {
                    let text = self.repo.file_at(&own, &p)?.map(|b| String::from_utf8(b).with_context(|| format!("{p}: not text"))).transpose()?;
                    nix.insert(p, text);
                }
            }
        }
        let files = self.repo.tree_files(&self.schema, &base)?;
        let lock = self.repo.file_at(&base, "flake.lock")?;
        let base_nix = nix.keys().map(|p| Ok((p.clone(), self.repo.file_at(&base, p)?))).collect::<Result<_>>()?;
        Ok(Against { base, files, lock, data, new_lock, nix, base_nix })
    }

    fn commit(&mut self, o: CommitOptions) -> Result<String> {
        let s = self.schema.clone();
        let head = self.repo.head()?;
        let a = self.against()?;
        let base = a.base.clone();
        let changes = a.changes(&s)?;
        let files: Vec<(String, Option<String>)> = changes.iter().map(|(p, _, n)| (p.clone(), n.clone())).collect();
        if o.check {
            if base != head {
                bail!("the configuration has changed since your candidate was started: `rollback 0` starts over");
            }
            // The rules between settings, as `commit` applies them.
            crate::whole::software_allowed(&a.data)?;
            self.machine.check(&base, &files)?;
            return Ok(self.said("configuration check succeeds"));
        }
        // A commit confirmed waiting freezes every other change (the broker refuses them): say so
        // before evaluating, asking or proposing anything.
        let waiting = self.machine.pending_confirm()?;
        if changes.is_empty() {
            let Some(w) = waiting else { return Ok(self.said("commit complete (no changes)")) };
            if o.confirm.is_some() {
                bail!("change #{} is already waiting for confirmation until {}: `commit` confirms it", w.seq, w.deadline);
            }
            // As on network devices: committing again within the window confirms it — but only its author may.
            if w.by.as_deref().is_some_and(|by| by != self.me) {
                bail!("change #{} is waiting for {} to confirm it; it rolls back at {} unless they do", w.seq, w.by.as_deref().unwrap_or("?"), w.deadline);
            }
            let n = self.machine.confirm()?;
            return Ok(self.said(&format!("commit complete: change #{} confirmed (#{n})", w.seq)));
        }
        if let Some(w) = waiting {
            bail!(
                "change #{} (commit confirmed) is waiting until {}: nothing else can change until it is confirmed \
                 (`commit` with no changes) or rolled back",
                w.seq, w.deadline
            );
        }
        if base != head {
            bail!(
                "the configuration has changed since your candidate was started ({} → {}): \
                 `rollback 0` starts over from the current configuration",
                short(&base),
                short(&head)
            );
        }
        crate::whole::software_allowed(&a.data)?;
        let tier = self.tier_of(&changes);
        // A person approves at a terminal: a system change, and a change to someone else's folder.
        let others = self.others_folders(&changes);
        self.machine.check(&base, &files)?;
        let compare = a.compare(&self.whole())?;
        let mut shown: String = compare.lines().map(|l| format!("{}\n", crate::history::clean(l))).collect();
        // The comparison as it is; the sentences around it fit the window (ui.rs).
        let ui = self.ui();
        // SSH admits keys only unless passwords are allowed (modules/services.nix, however it is turned
        // on): keys only and none, nobody could log in; with some, approving it approves exactly those
        // (as the changed configuration has them). Whether this turns SSH on is read from the
        // evaluated configurations, before and after: every way counts. Only tree and Nix files can.
        // Any system-tier change can (a software update too: TorqOS's own modules may change how SSH is
        // set up; audit of 2026-10-31) — only a person's own data files cannot.
        let ssh_relevant = files.iter().any(|(p, _)| self.schema.tier_of(p) != Some("user"));
        if ssh_relevant {
            let states = self.machine.ssh(&base, &files)?;
            shown.push_str(&ssh_review(self.machine, states, &base, &files, "committed", &ui)?);
        }
        // How the machine boots: a mistake shows only at the next restart (no `commit confirmed` timer runs
        // on a system that does not come up), so say so before anyone approves it.
        let section = |l: &str| l.strip_prefix("[edit ").and_then(|s| s.strip_suffix(']')).and_then(|s| s.split(' ').next()).map(String::from);
        let boots = compare.lines().filter_map(section).any(|top| matches!(top.as_str(), "fileSystems" | "boot" | "swapDevices"))
            || files.iter().any(|(p, _)| p == "hardware-configuration.nix");
        // A folder as the source: the approval is of its content as it is now (the lock's hash).
        if a.data.source.as_deref().is_some_and(crate::software::writable_folder) && files.iter().any(|(p, _)| p == "flake.nix" || p == "flake.lock") {
            shown.push_str(&format!("{}\n", ui.line("! ", "TorqOS comes from a folder (path:): whoever can write to it decides what the next `request system update` installs.")));
        }
        if boots {
            shown.push_str(&format!("{}\n", ui.line("! ", "This changes how the machine starts (file systems, boot): a mistake shows only at the next restart, where `commit confirmed` cannot undo it; the earlier configurations stay in the boot menu.")));
        }
        let why = match (tier, others.is_empty()) {
            ("system", _) => Some("This changes the system (all users).".to_string()),
            (_, false) => Some(format!("This changes the settings of: {}.", others.join(", "))),
            _ => None,
        };
        if let Some(why) = why {
            if let Some(stop) = self.machine.ask(&format!("{shown}{} ", ui.line("", &format!("{why} Commit?")))).go_on("committed")? {
                return Ok(stop);
            }
        }
        let (confirm, comment) = (o.confirm, o.comment);
        let scratch = tempfile::tempdir()?;
        let diff = candidate::unified_diff(scratch.path(), &changes)?;
        // One line of text, as the broker takes it (a tab or an escape typed in a comment: shown, not sent).
        let reason = crate::history::one_line(&comment.unwrap_or_else(|| summary(&compare)), usize::MAX);
        let seq = self.machine.commit(&diff, &base, tier, &reason, confirm)?;
        self.discard()?;
        Ok(match confirm {
            Some(m) => format!(
                "{}{}",
                self.said(&format!("commit confirmed will be automatically rolled back in {m} minute{} unless confirmed (change #{seq})", if m == 1 { "" } else { "s" })),
                self.said("commit complete")
            ),
            None => self.said(&format!("commit complete (change #{seq})")),
        })
    }

    /// `file list|show|edit|delete`: the config repo's own Nix files, the escape hatch for what is
    /// not a value (docs/CONFIG-IS-THE-DEVICE.md). In configuration mode, the candidate's (edits
    /// included); otherwise, the active configuration's. Edits wait in the candidate for `commit`.
    fn file(&mut self, action: FileAction, path: Option<String>) -> Result<String> {
        let editing = self.mode == Mode::Configuration;
        let (rev, nix, attached) = if editing {
            let d = self.candidate_data()?;
            let c = self.candidate()?;
            (c.base.clone(), c.nix.clone(), d.attached)
        } else {
            let head = self.repo.head()?;
            let attached = self.configuration_of(&head)?.attached;
            (head, Default::default(), attached)
        };
        let text_of = |s: &Self, p: &str| -> Result<Option<String>> {
            match nix.get(p) {
                Some(t) => Ok(t.clone()),
                None => s.repo.file_at(&rev, p)?.map(|b| String::from_utf8(b).with_context(|| format!("{p}: not text"))).transpose(),
            }
        };
        let Some(path) = path else {
            // `file list`: every Nix file, where each is attached, and what the candidate changed.
            let mut all: std::collections::BTreeSet<String> = self.repo.paths(&rev)?.into_iter().filter(|p| p.ends_with(".nix")).collect();
            all.extend(nix.keys().cloned());
            let mut out = String::new();
            for p in all {
                let state = match nix.get(&p) {
                    Some(None) => " (deleted in the candidate)",
                    Some(Some(_)) if self.repo.file_at(&rev, &p)?.is_none() => " (new in the candidate)",
                    Some(Some(_)) => " (edited in the candidate)",
                    None => "",
                };
                let at: Vec<&str> = attached.iter().filter(|(_, f)| f.contains(&p)).map(|(a, _)| a.as_str()).collect();
                let at: Vec<&str> = at.into_iter().map(|a| if a.is_empty() { "the top" } else { a }).collect();
                let at = if at.is_empty() { String::new() } else { format!("  [attached at: {}]", at.join("; ")) };
                let at = if p == "flake.nix" { "  (TorqOS's own: it builds the machine from the tree)".to_string() } else { at };
                out.push_str(&format!("{p}{state}{at}\n"));
            }
            return Ok(out);
        };
        crate::attach::valid(&path)?;
        match action {
            FileAction::Show => match text_of(self, &path)? {
                Some(t) => Ok(if t.ends_with('\n') || t.is_empty() { t } else { t + "\n" }),
                None => bail!("{path}: no such file in the configuration"),
            },
            FileAction::Edit => {
                let was = text_of(self, &path)?;
                let start = was.clone().unwrap_or_else(|| new_nix_file(&path));
                let Some(mut now) = self.machine.edit(&path, &start)? else { return Ok(self.said(&format!("{path}: unchanged"))) };
                // Not Nix as written: edited again if the person wants (as visudo does); else kept as it
                // is, nothing typed lost — `commit` refuses it until it is Nix.
                let mut broken = None;
                loop {
                    if now.contains('\0') {
                        bail!("{path}: not text; not kept");
                    }
                    let Some(why) = self.machine.nix_syntax(&path, &now)? else { break };
                    match self.machine.ask(&format!("{path} is not Nix as written: {why}\nEdit it again? ")) {
                        Answer::Yes => now = self.machine.edit(&path, &now)?.unwrap_or(now),
                        Answer::No | Answer::NobodyToAsk => {
                            broken = Some(why.lines().next().unwrap_or_default().to_string());
                            break;
                        }
                    }
                }
                if Some(&now) == was.as_ref() {
                    return Ok(self.said(&format!("{path}: unchanged")));
                }
                self.candidate()?.nix.insert(path.clone(), Some(now));
                self.save()?;
                let hint = if crate::attach::files(&attached).any(|f| *f == path) || self.repo.file_at(&rev, &path)?.is_some() {
                    String::new()
                } else {
                    format!(" (not attached anywhere yet: `set <place> config {path}`)")
                };
                if let Some(why) = broken {
                    return Ok(self.said(&format!("{path}: changed in the candidate, but not Nix yet ({why}): `file edit {path}` to fix it; `commit` refuses it until then")));
                }
                Ok(self.said(&format!("{path}: changed in the candidate{hint}; `show | compare` shows it, `commit` makes it so")))
            }
            FileAction::Delete => {
                if text_of(self, &path)?.is_none() {
                    bail!("{path}: no such file in the configuration");
                }
                if let Some((at, _)) = attached.iter().find(|(_, f)| f.contains(&path)) {
                    let place = if at.is_empty() { String::new() } else { format!("{at} ") };
                    bail!("{path} is attached at {}: `delete {place}config {path}` first", if at.is_empty() { "the top" } else { at.as_str() });
                }
                let exists = self.repo.file_at(&rev, &path)?.is_some();
                let c = self.candidate()?;
                if exists { c.nix.insert(path.clone(), None) } else { c.nix.remove(&path) };
                self.save()?;
                Ok(self.said(&format!("{path}: deleted in the candidate")))
            }
            FileAction::List => unreachable!("file list has no path"),
        }
    }

    /// `save <file>`: the whole candidate, from the top, and the content of every config file attached
    /// in it, so the file alone recreates the device (saved.rs). Private (0600): a configuration can
    /// hold secrets.
    fn save_to(&mut self, file: &str) -> Result<String> {
        let d = self.candidate_data()?;
        let tree = self.whole().show(&d, &[])?;
        let (base, nix) = {
            let c = self.candidate()?;
            (c.base.clone(), c.nix.clone())
        };
        // Every Nix file of the configuration as the candidate has it (not only the attached ones: one
        // may import another), except TorqOS's own flake.nix.
        let mut paths: std::collections::BTreeSet<String> = self.repo.paths(&base)?.into_iter().filter(|p| p.ends_with(".nix") && p != "flake.nix").collect();
        paths.extend(nix.keys().cloned());
        let (mut files, mut deleted) = (Vec::new(), Vec::new());
        for rel in paths {
            // Only names `load` reads back: a saved configuration always loads (audit of 2026-10-31).
            crate::attach::valid(&rel).with_context(|| format!("{rel}: a Nix file of the configuration whose name a saved configuration cannot hold"))?;
            let content = match nix.get(&rel) {
                Some(t) => t.clone(),
                None => self.repo.file_at(&base, &rel)?.map(|b| String::from_utf8(b).with_context(|| format!("{rel}: not text"))).transpose()?,
            };
            match content {
                Some(content) => files.push((rel, content)),
                // Deleted in the candidate (and there at its base): saved as a deletion, not left out.
                None if self.repo.file_at(&base, &rel)?.is_some() => deleted.push(rel),
                None => {}
            }
        }
        for rel in crate::attach::files(&d.attached) {
            if !files.iter().any(|(p, _)| p == rel) {
                bail!("{rel} is attached but not in the configuration: `file edit {rel}`, or `delete <place> config`");
            }
        }
        let text = crate::saved::render(&tree, &files, &deleted);
        if text.len() > crate::saved::MAX {
            bail!("the configuration is {} bytes; a saved one is at most {}, so it could not be loaded", text.len(), crate::saved::MAX);
        }
        let path = in_home(file);
        write_private(&path, text.as_bytes())?;
        let n = text.lines().count();
        Ok(self.said(&format!("Wrote {n} line{} of configuration to {}", if n == 1 { "" } else { "s" }, path.display())))
    }

    /// `load override|merge|set <file>`: a saved configuration into the candidate, from the top — all
    /// of it or, if any part is refused, none of it (the candidate is left as it was).
    fn load_from(&mut self, how: LoadHow, from: &LoadFrom) -> Result<String> {
        // A configuration is small: a file far bigger is not one, and is never read whole into memory
        // (read at most one byte past the limit, whatever the file does meanwhile).
        let (file, text) = match from {
            LoadFrom::File(file) => {
                let mut text = String::new();
                std::io::Read::read_to_string(&mut std::io::Read::take(std::fs::File::open(in_home(file))?, crate::saved::MAX as u64 + 1), &mut text)?;
                (file.as_str(), text)
            }
            LoadFrom::Terminal => ("terminal", self.machine.read_terminal(crate::saved::MAX + 1).context("load terminal")?),
        };
        if text.len() > crate::saved::MAX {
            bail!("{file} is not a saved configuration (bigger than the most, {} bytes)", crate::saved::MAX);
        }
        // Nothing to load is never a configuration (an override would empty the candidate).
        if text.trim().is_empty() {
            bail!("load {file}: nothing loaded: {}", if file == "terminal" { "nothing was pasted" } else { "the file is empty" });
        }
        let before = (self.candidate()?.clone(), self.edit.clone());
        let result = match how {
            LoadHow::Set => self.load_set(&text),
            LoadHow::Override | LoadHow::Merge => self.load_tree(&text, how == LoadHow::Override),
        };
        self.edit = before.1;
        match result {
            Ok(out) => Ok(out),
            Err(e) => {
                self.candidate = Some(before.0);
                self.save()?;
                Err(e.context("nothing loaded"))
            }
        }
    }

    /// `load set`: each line a `set` or `delete` command, from the top (what `| display set` writes).
    fn load_set(&mut self, text: &str) -> Result<String> {
        self.edit.clear();
        let mut n = 0;
        for (i, line) in text.lines().enumerate() {
            let w = words(line).with_context(|| format!("line {}", i + 1))?;
            match w.first().map(String::as_str) {
                None => continue,
                Some(c) if c.starts_with('#') => continue,
                Some("set" | "delete") => {}
                Some(_) => bail!("line {}: only `set` and `delete` lines load (`| display set` writes them)", i + 1),
            }
            self.exec_words(&w).with_context(|| format!("line {}: {}", i + 1, crate::history::one_line(line, 80)))?;
            n += 1;
        }
        let d = self.candidate_data()?;
        Ok(format!("{}{}", self.said(&format!("load complete ({n} line{})", if n == 1 { "" } else { "s" })), self.rules_note(&d)))
    }

    /// `load override` (the file is the candidate) or `load merge` (the file's settings set onto it).
    fn load_tree(&mut self, text: &str, whole: bool) -> Result<String> {
        let saved = crate::saved::parse(text)?;
        let cur = self.candidate_data()?;
        let mut d = if whole { Data { source: cur.source.clone(), ..Default::default() } } else { cur.clone() };
        for (path, v) in &saved.entries {
            let w = crate::saved::set_words(path, v);
            self.whole().set(&mut d, &w).with_context(|| format!("set {}", w.iter().map(|x| syntax::quote(x)).collect::<Vec<_>>().join(" ")))?;
        }
        let base = self.candidate()?.base.clone();
        let mut nix = if whole { Default::default() } else { self.candidate()?.nix.clone() };
        for (rel, content) in &saved.files {
            let at_base = self.repo.file_at(&base, rel)?;
            if at_base.as_deref() == Some(content.as_bytes()) {
                nix.remove(rel);
            } else {
                nix.insert(rel.clone(), Some(content.clone()));
            }
        }
        for rel in &saved.deleted {
            if self.repo.file_at(&base, rel)?.is_some() {
                nix.insert(rel.clone(), None);
            } else {
                nix.remove(rel);
            }
        }
        // Every attached file has its content. Override: in the file itself, never this machine's own
        // (a cut-short file must not load other Nix than it was saved with; audit of 2026-10-31).
        // Merge: in the file, or the configuration it is merged into.
        for rel in crate::attach::files(&d.attached) {
            let in_file = saved.files.iter().any(|(p, _)| p == rel);
            if whole && !in_file {
                bail!("{rel} is attached but its content is not in the file (a saved configuration carries its config files)");
            }
            if !nix.get(rel).is_some_and(Option::is_some) && self.repo.file_at(&base, rel)?.is_none() {
                bail!("{rel} is attached but its content is neither in the file nor in the configuration");
            }
        }
        // A new source is locked as `set` locks it (what `commit` installs is shown first).
        let mut out = String::new();
        if d.source != cur.source {
            out = self.lock_source(d.source.as_deref().unwrap_or_default(), false)?;
        }
        let note = self.rules_note(&d);
        self.put_data(d)?;
        self.candidate()?.nix = nix;
        self.save()?;
        out.push_str(&self.said("load complete"));
        out.push_str(&note);
        Ok(out)
    }

    /// The tier a set of changed files is: user if every one is a user-tier node's file.
    fn tier_of(&self, changes: &[(String, Option<&[u8]>, Option<String>)]) -> &'static str {
        let user = changes.iter().all(|(p, _, _)| self.schema.tier_of(p) == Some("user"));
        if user { "user" } else { "system" }
    }

    /// The users other than you whose folders (users/<name>/) the changes touch.
    fn others_folders(&self, changes: &[(String, Option<&[u8]>, Option<String>)]) -> Vec<String> {
        let mut names: Vec<String> = changes
            .iter()
            .filter_map(|(p, _, _)| p.strip_prefix("users/")?.split('/').next().map(String::from))
            .filter(|n| *n != self.me)
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Write `bytes` to `path`, private (0600) from the first byte: a new file beside it, then renamed
/// over it, so a reader never sees it half written or with another mode, and a link at `path` is
/// replaced, never written through. Anything there but a plain file (a device, a folder) is refused.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if !m.file_type().is_file() && !m.file_type().is_symlink() {
            bail!("{} is not a plain file: not written over", path.display());
        }
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().context("a file name")?.to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    let written = std::io::Write::write_all(&mut f, bytes).and_then(|_| f.sync_all()).and_then(|_| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    Ok(written?)
}

/// The tree data in a commit's files: the kept files, nixos/*.json, config-files.json.
fn data_of(s: &Schema, files: &std::collections::BTreeMap<String, Vec<u8>>) -> Result<Data> {
    let config = s.load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let settings = crate::settings::load(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())))?;
    let attached = crate::attach::load(files.get(crate::attach::FILE).map(Vec::as_slice))?;
    let source = files.get("flake.nix").and_then(|b| crate::software::source_of(&String::from_utf8_lossy(b)));
    Ok(Data { config, settings, attached, source })
}

/// The raw change between two commits: every file (the tree's, the Nix files, `flake.lock`), byte
/// for byte as committed (not as TorqOS would write them now).
fn commits_diff(old: &std::collections::BTreeMap<String, Vec<u8>>, new: &std::collections::BTreeMap<String, Vec<u8>>) -> Result<String> {
    let paths: std::collections::BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let changes: Vec<candidate::RawChange> = paths
        .into_iter()
        .filter(|p| old.get(*p) != new.get(*p))
        .map(|p| (p.clone(), old.get(p).map(Vec::as_slice), new.get(p).map(Vec::as_slice)))
        .collect();
    let scratch = tempfile::tempdir()?;
    candidate::unified_diff_bytes(scratch.path(), &changes)
}

/// The candidate and its base, read once: the base's tree files and `flake.lock`, the candidate's.
struct Against {
    base: String,
    files: std::collections::BTreeMap<String, Vec<u8>>,
    lock: Option<Vec<u8>>,
    data: Data,
    new_lock: Option<String>,
    /// Config files as edited in the candidate, and as they are at the base (None: not there).
    nix: std::collections::BTreeMap<String, Option<String>>,
    base_nix: std::collections::BTreeMap<String, Option<Vec<u8>>>,
}

impl Against {
    fn base_data(&self, s: &Schema) -> Result<Data> {
        data_of(s, &self.files)
    }

    /// What the candidate changes: the tree's files (every kind), the config files edited, then the
    /// software. A NixOS settings file changes when its values do, not its formatting.
    fn changes(&self, s: &Schema) -> Result<Vec<candidate::Change<'_>>> {
        let mut c = candidate::changes(s, &self.files, &self.data.config)?;
        let base = self.base_data(s)?;
        let new = crate::settings::files(&self.data.settings);
        let tops: std::collections::BTreeSet<&String> = base.settings.keys().chain(self.data.settings.keys()).collect();
        for top in tops {
            let empty = |v: Option<&serde_json::Value>| v.is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()));
            let (was, now) = (base.settings.get(top), self.data.settings.get(top));
            if was == now || (empty(was) && empty(now)) {
                continue;
            }
            let path = format!("{}{top}.json", crate::settings::DIR);
            c.push((path.clone(), self.files.get(&path).map(Vec::as_slice), new.get(&path).cloned()));
        }
        // The software's source: the one line of flake.nix (its lock is the candidate's, below).
        if let (Some(src), true) = (&self.data.source, base.source != self.data.source) {
            let was = self.files.get("flake.nix").context("this machine has no flake.nix")?;
            let now = crate::software::with_source(&String::from_utf8_lossy(was), src)?;
            c.push(("flake.nix".to_string(), Some(was.as_slice()), Some(now)));
        }
        if base.attached != self.data.attached {
            let path = crate::attach::FILE.to_string();
            c.push((path.clone(), self.files.get(&path).map(Vec::as_slice), crate::attach::render(&self.data.attached)));
        }
        for (path, text) in &self.nix {
            let was = self.base_nix.get(path).cloned().flatten();
            if was.as_deref() != text.as_deref().map(str::as_bytes) {
                c.push((path.clone(), self.base_nix.get(path).and_then(|b| b.as_deref()), text.clone()));
            }
        }
        c.extend(candidate::lock_change(self.lock.as_deref(), &self.new_lock));
        Ok(c)
    }

    /// `show | compare`: the tree's changes by NixOS path, the config files edited as their raw diffs
    /// (as written: Nix is shown, never interpreted), then the software's.
    fn compare(&self, w: &crate::whole::Whole) -> Result<String> {
        let mut out = w.compare(&self.base_data(w.schema)?, &self.data);
        let edited = self.edited_files();
        if !edited.is_empty() {
            let scratch = tempfile::tempdir()?;
            out.push_str(&format!("[edit config files]\n{}", candidate::unified_diff(scratch.path(), &edited)?));
        }
        out.push_str(&software_compare(self.lock.as_deref(), &self.new_lock));
        Ok(out)
    }

    /// The config files the candidate changes (None: deleted), with what they were.
    fn edited_files(&self) -> Vec<candidate::Change<'_>> {
        self.nix
            .iter()
            .filter(|(p, t)| self.base_nix.get(*p).cloned().flatten().as_deref() != t.as_deref().map(str::as_bytes))
            .map(|(p, t)| (p.clone(), self.base_nix.get(p).and_then(|b| b.as_deref()), t.clone()))
            .collect()
    }

    /// How many changes the candidate holds: each setting added, removed or changed once (a changed
    /// value is a `-` and a `+` of one setting), each software input moved, each config file
    /// changed once — however many lines its diff has.
    fn count(&self, w: &crate::whole::Whole) -> Result<usize> {
        let settings = w.compare(&self.base_data(w.schema)?, &self.data) + &software_compare(self.lock.as_deref(), &self.new_lock);
        Ok(settings_changed(&settings) + self.edited_files().len())
    }
}

/// The software a candidate changes, as `show | compare` shows it: each input moved, and a
/// downgrade said so.
fn software_compare(old: Option<&[u8]>, new: &Option<String>) -> String {
    let Some(new) = new else { return String::new() };
    let old = old.map(|o| String::from_utf8_lossy(o).into_owned()).unwrap_or_default();
    if old == *new {
        return String::new();
    }
    let mut out = String::from("[edit system software]\n");
    let moves = crate::pkg::lock_moves(&old, new);
    if moves.is_empty() {
        out.push_str("    flake.lock changes (`| display diff` shows how)\n");
    }
    let mut older = false;
    for m in &moves {
        // Names come from upstream flakes: one line each, cleaned, so none can draw a line of its own.
        let name = crate::history::one_line(&m.name, 40);
        let (before, after) = crate::pkg::move_dates(m);
        if m.was {
            out.push_str(&format!("-   {name} {before};\n"));
        }
        if m.is {
            out.push_str(&format!("+   {name} {after};\n"));
        }
        // Older to the second; an input whose date is unknown on either side says nothing.
        older |= matches!((m.before, m.after), (Some(b), Some(a)) if a < b);
    }
    if older {
        out.push_str("## older software than now: a downgrade\n");
    }
    out
}

/// The sections of a comparison (`[edit <path>]` and its lines) at or below `path` — and the config
/// files' (`[edit config files]`: whole files, no path); all of them at the top. Nothing there but
/// changes elsewhere: said, so an empty answer never hides them.
fn sections_under(compare: &str, path: &[String]) -> String {
    if path.is_empty() {
        return compare.to_string();
    }
    let out = sections_at(compare, path);
    if out.is_empty() && !compare.trim().is_empty() {
        return "## nothing changes here; elsewhere it does: `top show | compare`\n".into();
    }
    out
}

fn sections_at(compare: &str, path: &[String]) -> String {
    let mut out = String::new();
    let mut keep = false;
    for line in compare.split_inclusive('\n') {
        if let Some(at) = line.trim_end().strip_prefix("[edit").and_then(|s| s.strip_suffix(']')) {
            // (Split as a line is: a name with spaces is quoted there.)
            keep = at.trim() == "config files" || words(at.trim()).is_ok_and(|w| w.starts_with(path));
        }
        if keep {
            out.push_str(line);
        }
    }
    out
}


/// A ledger reason from the comparison: its `+`/`-` lines, short.
fn summary(compare: &str) -> String {
    // Each `+`/`-` line with its section (`[edit torq packages user]` + `+   dev [ jq ];` →
    // `+ users users dev packages [ jq ];`): a ledger reason is read on its own.
    let mut section = String::new();
    let mut lines: Vec<String> = Vec::new();
    let mut was = String::new();
    for l in compare.lines() {
        if let Some(s) = l.strip_prefix("[edit").and_then(|s| s.strip_suffix(']')) {
            section = s.trim().to_string();
        } else if section == "config files" {
            // A raw diff: just which file, and how.
            if let Some(p) = l.strip_prefix("--- ") {
                was = p.strip_prefix("a/").unwrap_or(p).to_string();
            } else if let Some(p) = l.strip_prefix("+++ ") {
                lines.push(match p.strip_prefix("b/") {
                    Some(p) if was == "/dev/null" => format!("+ config file {p}"),
                    Some(p) => format!("~ config file {p}"),
                    None => format!("- config file {was}"),
                });
            }
        } else if let Some((sign, rest)) = l.split_at_checked(1).filter(|(s, _)| *s == "+" || *s == "-") {
            let at = if section.is_empty() { String::new() } else { format!("{section} ") };
            lines.push(format!("{sign} {at}{}", rest.trim()));
        } else if !l.starts_with('#') && !l.trim().is_empty() {
            lines.push(l.trim().to_string());
        }
    }
    let s = lines.join(" ");
    if s.chars().count() > 120 { format!("{}…", s.chars().take(119).collect::<String>()) } else { s }
}

fn short(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// The settings a comparison of settings changes (no config file's diff: Against::count): its `+`
/// and `-` lines, each setting (its section and name) once.
fn settings_changed(compare: &str) -> usize {
    let mut section = "";
    let mut seen = std::collections::BTreeSet::new();
    for line in compare.lines() {
        if line.starts_with('[') {
            section = line;
        } else if let Some(rest) = line.strip_prefix('+').or_else(|| line.strip_prefix('-')) {
            let name = rest.split_whitespace().next().unwrap_or_default();
            seen.insert((section, name));
        }
    }
    seen.len()
}

/// What a change does to who can log in over SSH, said before a person approves it (a commit, a
/// revert — `verb`), whenever it changes that, not only when SSH turns on (audit of 2026-11-01):
/// `states` is SSH before and after it (evaluated: every way of setting it counts).
/// - Passwords accepted after it, and newly (SSH just on, or passwords just allowed): said so, with
///   the keys if SSH just turned on.
/// - Keys only after it, and none: refused — nobody could log in, and getting back in needs the
///   console. Keys only, and newly so (SSH just on, passwords just turned off) or other keys than
///   before (a key removed, another owner): the keys, approved as they are.
pub fn ssh_review(m: &dyn Machine, states: (Ssh, Ssh), base: &str, files: &[(String, Option<String>)], verb: &str, ui: &crate::ui::Ui) -> Result<String> {
    let (was, now) = states;
    if !now.on {
        return Ok(String::new());
    }
    let listed = |owner: &str, keys: &[String]| if keys.is_empty() { String::new() } else { format!("Keys that will be able to log in as {owner}:\n{}\n", keys.join("\n")) };
    if now.passwords {
        if was.on && was.passwords {
            return Ok(String::new());
        }
        let mut out = format!("{}\n", ui.line("! ", "SSH will accept passwords as well as keys (services openssh settings PasswordAuthentication): repeated wrong guesses are blocked."));
        if !was.on {
            let (owner, keys) = m.ssh_keys(base, files)?;
            out.push_str(&listed(&owner, &keys));
        }
        return Ok(out);
    }
    let (owner, keys) = m.ssh_keys(base, files)?;
    if keys.is_empty() {
        let how = if was.on { "would let in" } else { "lets in" };
        bail!(
            "not {verb}: SSH here {how} {owner}'s keys only, and they have none, so nobody could log in; \
             add a public key to ~/.ssh/authorized_keys first, or allow passwords too \
             (`set services openssh settings PasswordAuthentication true`)"
        );
    }
    if was.on && !was.passwords && m.ssh_keys(base, &[])? == (owner.clone(), keys.clone()) {
        return Ok(String::new());
    }
    Ok(listed(&owner, &keys))
}

/// The software a configuration's lock names: its own inputs, each with when it is from
/// (`torq 2026-11-01, nixpkgs 2026-10-29`); None if it names none.
fn software_of(lock: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(lock).unwrap_or_default();
    // The machine's own inputs (what it follows from them is theirs).
    let direct: Vec<String> = v["nodes"]["root"]["inputs"].as_object().map(|o| o.iter().filter_map(|(_, n)| n.as_str().map(String::from)).collect()).unwrap_or_default();
    let shown: Vec<String> = crate::pkg::lock_moves("", lock)
        .iter()
        .filter(|m| direct.contains(&m.name))
        .map(|m| format!("{} {}", crate::history::one_line(&m.name, 30), crate::pkg::move_dates(m).1))
        .collect();
    (!shown.is_empty()).then(|| shown.join(", "))
}

/// One line for each configuration `rollback ?` offers, newest first (0, 1, …).
pub fn configuration_lines(h: &History) -> Vec<String> {
    h.configurations.iter().map(|c| format!("{} {}: {}", c.date, c.who, crate::history::one_line(&c.what, 80))).collect()
}

/// "1 change", "3 changes".
fn changes(n: usize) -> String {
    format!("{n} change{}", if n == 1 { "" } else { "s" })
}

/// A file named for `save` and `load`: as given if absolute, otherwise in the home folder (as on a
/// network device), wherever torqcli was started.
fn in_home(file: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(file);
    match std::env::var_os("HOME") {
        Some(home) if path.is_relative() => std::path::Path::new(&home).join(path),
        _ => path.to_path_buf(),
    }
}

/// A new config file as `file edit` starts it: an empty NixOS module, saying what it is and how it
/// is written (comments: Nix ignores them).
fn new_nix_file(path: &str) -> String {
    format!(
        "# {path}: part of this machine's configuration, for what the tree's settings cannot say\n\
         # (Nix code: functions, software built here). Written as in NixOS's configuration.nix:\n\
         #   services.httpd.enable = true;\n\
         # `set <place> config {path}` attaches it; `commit check` evaluates it.\n\
         {{ config, lib, pkgs, ... }}:\n{{\n}}\n"
    )
}

/// How well `term` (small letters) is found for `help`, in a command's or setting's `name` (its
/// words, `services openssh enable`) or its `help`: 4 as a whole part of the name (`hostname` is
/// hostName, `ssh` is not openssh), 3 at the beginning of one or of a word in it (`host`), 2 inside
/// one (`ssh` in openssh), 1 in the help only; None, not at all.
fn found_in(term: &str, name: &str, help: &str) -> Option<usize> {
    let parts: Vec<String> = name.split_whitespace().map(str::to_lowercase).collect();
    let words = words_in(name);
    if parts.iter().any(|p| p == term) {
        Some(4)
    } else if parts.iter().chain(&words).any(|w| w.starts_with(term)) {
        Some(3)
    } else if words.iter().any(|w| w.contains(term)) {
        // (Inside one word, never across two: `ntp` is not in mount|Point.)
        Some(2)
    } else if words_in(help).iter().any(|w| w.starts_with(term)) {
        Some(1)
    } else {
        None
    }
}

/// The words of a text as `help` searches them, small letters: split at anything not a letter or
/// digit, and inside names where a capital starts a word (`mountPoint`: mount, point).
fn words_in(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut last_lower = false;
    for c in text.chars() {
        if (!c.is_alphanumeric() || (c.is_uppercase() && last_lower)) && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if c.is_alphanumeric() {
            word.extend(c.to_lowercase());
        }
        last_lower = c.is_lowercase() || c.is_ascii_digit();
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::Path;
    use std::process::Command;

    /// Tree files a change writes (None: deletes), as the machine is given them.
    type Files = Vec<(String, Option<String>)>;

    #[derive(Default)]
    struct Fake {
        commits: RefCell<Vec<(String, String, String, String, Option<u32>)>>,
        checks: RefCell<Vec<Files>>,
        answer: bool,
        pending: Option<Waiting>,
        confirmed: RefCell<u32>,
        asked: RefCell<Vec<String>>,
        /// The broker refuses the commit.
        refuse: bool,
        /// The bases `request system update` was asked about.
        bases: RefCell<Vec<String>>,
        /// The ledger's configurations, if a test gives them; else the repo's history stands in.
        ledger: Option<Vec<Configuration>>,
        /// Where the ledger says the repo is, if a test says (else: its newest configuration).
        ledger_at: Option<String>,
        /// SSH before and after a commit, if a test says.
        /// A person answers no (else, with `answer` false, nobody is there to ask).
        no: bool,
        ssh_state: Option<(Ssh, Ssh)>,
        /// The keys turning SSH on would admit, and what was asked about.
        keys: Vec<String>,
        /// The keys before the change, where a test says they differ (else: as after it).
        keys_before: Option<Vec<String>>,
        ssh_asked: RefCell<Vec<Files>>,
        /// What `file edit` leaves a file as (None: unchanged), and what it was given.
        edit_to: Option<String>,
        edit_again: Option<String>,
        /// What `load … terminal` reads (None: no terminal).
        typed: Option<String>,
        edited: RefCell<Vec<(String, String)>>,
        ssh_calls: RefCell<usize>,
        /// The flake.nix each source was locked with.
        flakes: RefCell<Vec<String>>,
        /// The newest TorqOS is this commit of the releases, signed by the release key or not (else:
        /// `NEWER`, from no releases); and the commits whose signature was checked.
        release: Option<(String, bool)>,
        release_checks: RefCell<Vec<String>>,
    }
    impl Machine for Fake {
        fn check(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<()> {
            self.checks.borrow_mut().push(files.to_vec());
            Ok(())
        }
        fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
            if self.refuse {
                bail!("commit refused: busy");
            }
            self.commits.borrow_mut().push((diff.into(), base.into(), tier.into(), reason.into(), confirm));
            Ok(42)
        }
        fn pending_confirm(&self) -> Result<Option<Waiting>> {
            Ok(self.pending.clone())
        }
        fn confirm(&self) -> Result<u64> {
            *self.confirmed.borrow_mut() += 1;
            Ok(43)
        }
        /// (A test's "no" is a script's: nobody to ask; `no` says a person declined.)
        fn ask(&self, q: &str) -> Answer {
            self.asked.borrow_mut().push(q.into());
            match (self.answer, self.no) {
                (true, _) => Answer::Yes,
                (false, true) => Answer::No,
                (false, false) => Answer::NobodyToAsk,
            }
        }
        /// (The first edit leaves `edit_to`; any later one, `edit_again`.)
        fn edit(&self, path: &str, text: &str) -> Result<Option<String>> {
            self.edited.borrow_mut().push((path.into(), text.into()));
            Ok(if self.edited.borrow().len() > 1 && self.edit_again.is_some() { self.edit_again.clone() } else { self.edit_to.clone() })
        }
        /// (Not Nix: a text with an unclosed `{`, as far as a test needs.)
        fn nix_syntax(&self, path: &str, text: &str) -> Result<Option<String>> {
            Ok((text.matches('{').count() != text.matches('}').count()).then(|| format!("unexpected end of file, at {path}:3:1")))
        }
        fn read_terminal(&self, _: usize) -> Result<String> {
            self.typed.clone().context("no terminal")
        }
        fn alarms(&self, detail: bool, _: Option<usize>) -> Result<String> {
            Ok(if detail { "all well\n  running system /nix/store/x\n".into() } else { "all well\n".into() })
        }
        fn search(&self, terms: &[String], _: &std::collections::BTreeSet<String>, _: Option<usize>) -> Result<String> {
            Ok(format!("searched {}\n", terms.join(" ")))
        }
        fn repair(&self) -> Result<String> {
            Ok("repaired\n".into())
        }
        fn set_password(&self, user: &str) -> Result<String> {
            Ok(format!("password of {user}\n"))
        }
        fn ledger(&self, count: usize, fit: Option<usize>) -> Result<String> {
            Ok(format!("ledger {count} fitting {fit:?}\n"))
        }
        fn facts(&self) -> Result<crate::facts::Facts> {
            Ok(crate::facts::Facts { nixos: "26.05".into(), kernel: "6.18".into(), uptime: 60, generations: 2, ..Default::default() })
        }
        /// As the evaluator would: `services openssh|sshd enable true` in the changed files, or the old
        /// list's `ssh`; passwords only where PasswordAuthentication is true (modules/services.nix: keys
        /// only otherwise). A test may say otherwise.
        fn ssh(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<(Ssh, Ssh)> {
            *self.ssh_calls.borrow_mut() += 1;
            if let Some(s) = self.ssh_state {
                return Ok(s);
            }
            let has = |path: &str, f: &dyn Fn(&serde_json::Value) -> bool| {
                files.iter().any(|(p, c)| p == path && c.as_deref().and_then(|c| serde_json::from_str(c).ok()).is_some_and(|v| f(&v)))
            };
            let yes = Some(&serde_json::Value::Bool(true));
            let old = has("services.json", &|v| v.as_array().is_some_and(|a| a.iter().any(|x| x == "ssh")));
            let nixos = has("nixos/services.json", &|v| v.pointer("/openssh/enable") == yes || v.pointer("/sshd/enable") == yes);
            let passwords = has("nixos/services.json", &|v| v.pointer("/openssh/settings/PasswordAuthentication") == yes);
            Ok((Ssh::default(), Ssh { on: old || nixos, passwords }))
        }
        fn ssh_keys(&self, _base: &str, files: &[(String, Option<String>)]) -> Result<(String, Vec<String>)> {
            self.ssh_asked.borrow_mut().push(files.to_vec());
            match (&self.keys_before, files.is_empty()) {
                (Some(before), true) => Ok(("dev".into(), before.clone())),
                _ => Ok(("dev".into(), self.keys.clone())),
            }
        }
        fn revert(&self, seq: u64, me: &str) -> Result<String> {
            Ok(format!("reverted {seq} for {me}\n"))
        }
        fn configurations(&self, repo: &Repo, count: usize) -> Result<History> {
            let configurations: Vec<Configuration> = match &self.ledger {
                Some(l) => l.iter().take(count).cloned().collect(),
                None => repo
                    .history(count)?
                    .into_iter()
                    .map(|c| Configuration { id: c.id, date: c.date, who: "dev".into(), what: c.subject, seq: None })
                    .collect(),
            };
            let at = self.ledger_at.clone().or_else(|| configurations.first().map(|c| c.id.clone()));
            Ok(History { configurations, at })
        }
        fn update_lock(&self, base: &str, all: bool) -> Result<String> {
            self.bases.borrow_mut().push(base.into());
            Ok(self.newest(all))
        }
        fn lock_with(&self, base: &str, flake_nix: &str, all: bool) -> Result<String> {
            self.bases.borrow_mut().push(base.into());
            self.flakes.borrow_mut().push(flake_nix.into());
            Ok(self.newest(all))
        }
        fn check_release(&self, rev: &str) -> Result<String> {
            self.release_checks.borrow_mut().push(rev.into());
            match &self.release {
                Some((r, true)) if r == rev => Ok("Good \"git\" signature for torqos-release".into()),
                _ => bail!("release {} is not signed by the TorqOS release key", &rev[..12]),
            }
        }
    }
    impl Fake {
        /// What locking finds newest: the release, if a test gives one.
        fn newest(&self, all: bool) -> String {
            match &self.release {
                Some((rev, _)) => format!(r#"{{"nodes":{{"root":{{"inputs":{{"torq":"torq"}}}},"torq":{{"locked":{{"type":"git","url":"https://github.com/netstacks/torqos-releases","rev":"{rev}","lastModified":{NEWER},"narHash":"r"}}}}}},"version":7}}"#),
                None => lock(if all { NEWER_ALL } else { NEWER }),
            }
        }
    }

    fn sh(dir: &Path, script: &str) {
        let st = Command::new("sh").arg("-c").arg(script).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").status().unwrap();
        assert!(st.success(), "{script}");
    }
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        sh(d.path(), "git init -q -b main && git config user.name t && git config user.email t@t \
            && mkdir -p system && printf '{ \"host-name\": \"one\" }\\n' > system/host.json && git add -A && git commit -q -m first \
            && printf '{ \"host-name\": \"two\" }\\n' > system/host.json && git commit -qam second");
        d
    }
    fn w(s: &str) -> Vec<String> {
        words(s).unwrap()
    }
    fn nixos_tree() -> crate::nixos::Tree {
        crate::nixos::Tree::parse(include_str!("nixos-fixture.json")).unwrap()
    }
    fn session<'a>(d: &Path, saved: Option<PathBuf>, m: &'a Fake) -> Session<'a> {
        Session::new(Schema::parse(include_str!("schema-fixture.json")).unwrap(), nixos_tree(), Repo::new(d), saved, m, "dev")
    }
    fn waiting(by: &str) -> Option<Waiting> {
        Some(Waiting { seq: 42, deadline: "soon".into(), by: Some(by.into()) })
    }
    fn run(s: &mut Session, lines: &[&str]) -> String {
        lines.iter().map(|l| s.exec(l).unwrap_or_else(|e| panic!("{l}: {e:#}"))).collect()
    }


    /// `rollback 0 <path>` undoes the changes at one place, the rest of the candidate as it is; `rollback
    /// ?` lists the configurations, as on a network device.
    #[test]
    fn one_change_undone() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set networking hostName three", "set time timeZone UTC"]);
        assert_eq!(s.exec("rollback 0 networking hostName").unwrap(), "Put back networking hostName as the configuration in effect has it: 1 change undone.\n");
        let left = s.exec("show | compare").unwrap();
        assert!(left.contains("timeZone UTC") && !left.contains("hostName"), "{left}");
        assert_eq!(s.exec("rollback 0 netw hostN").unwrap(), "Put back networking hostName as the configuration in effect has it: it already was.\n", "abbreviated, as anywhere");
        s.exec("edit time").unwrap();
        assert!(s.exec("rollback 0 timeZone").unwrap().contains("Put back time timeZone"), "from where `edit` is");
        assert_eq!(s.exec("top show | compare").unwrap(), "No changes: the candidate is the configuration in effect.\n");
    }

    /// A question answered no is a choice, said ("Not deleted."), not a failure; with nobody to ask (a
    /// script), refused.
    #[test]
    fn no_is_an_answer() {
        let (d, m) = (repo(), Fake { no: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "edit time", "delete"]), "Entering configuration mode\nNot deleted.\n");
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "delete"));
        assert_eq!(e, "not deleted: this needs a person to approve it at a terminal");
    }

    /// Every command says what it did, and every way out says what is kept: rollback (what it loaded or
    /// discarded), an empty comparison, a place with nothing set, configure with changes left from
    /// before, exit with them kept, save and load in the home folder.
    #[test]
    fn every_command_says_what_it_did() {
        let (d, m) = (repo(), Fake::default());
        let home = tempfile::tempdir().unwrap();
        let saved = home.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        assert_eq!(run(&mut s, &["configure", "rollback 0"]), "Entering configuration mode\nNo changes to discard: the candidate is the configuration in effect.\n");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.\n");
        assert_eq!(s.exec("show time").unwrap(), "Nothing is set at time.\n");
        s.exec("set time timeZone UTC").unwrap();
        assert!(s.exec("exit").unwrap().starts_with("The candidate has changes not yet committed: they are kept ("));
        assert_eq!(s.exec("exit").unwrap(), "The candidate has changes not yet committed: they are kept for your next `configure`.\n");
        let mut again = session(d.path(), Some(saved), &m);
        assert_eq!(again.exec("configure").unwrap(), "Entering configuration mode\nThe candidate has changes not yet committed: `show | compare` lists them, `rollback 0` discards them.\n");
        assert_eq!(again.exec("rollback 0").unwrap(), "Discarded 1 change: the candidate is the configuration in effect.\n");
        assert_eq!(settings_changed("[edit networking]\n-   hostName two;\n+   hostName one;\n+   domain x;\n[edit time]\n+   timeZone UTC;\n"), 3, "a changed value is one change");
        assert_eq!(in_home("/etc/x.conf"), std::path::PathBuf::from("/etc/x.conf"));
        if let Some(h) = std::env::var_os("HOME") {
            assert_eq!(in_home("mine.conf"), std::path::Path::new(&h).join("mine.conf"), "as on a network device: the home folder");
        }
    }

    /// `help`: the commands of the mode; `help <command>`: that command and what may follow it;
    /// `help <words>`: commands and settings with those words, closest first.
    #[test]
    fn help_lists_explains_and_searches() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        let all = s.exec("help").unwrap();
        for w in ["configure", "file", "help", "request", "show"] {
            assert!(all.contains(&format!("\n  {w} ")), "{w} in: {all}");
        }
        let one = s.exec("help show system").unwrap();
        assert!(one.starts_with("show system: This machine.") && one.contains("alarms") && one.contains("rollback"), "{one}");
        assert_eq!(s.exec("help sh sys").unwrap(), one, "abbreviated, as everywhere");
        let found = s.exec("help commit").unwrap();
        assert!(found.contains("Commands:\n  show system commit") && found.contains("\n  configure "), "the name first, then the help: {found}");
        assert!(found.contains("Commands in configuration mode:\n  commit "), "{found}");
        let none = s.exec("help zzqqx").unwrap();
        assert!(none.starts_with("Nothing has \"zzqqx\" in its name or description."), "{none}");
        // How a word is found: a whole part of a name, its beginning, inside it, or in the help only.
        assert_eq!(found_in("hostname", "networking hostName", ""), Some(4));
        assert_eq!(found_in("host", "networking hostName", ""), Some(3));
        assert_eq!(found_in("ssh", "services openssh enable", ""), Some(2));
        assert_eq!(found_in("daemon", "services openssh enable", "Whether to enable the OpenSSH daemon."), Some(1));
        assert_eq!(found_in("ntp", "fileSystems <name> mountPoint", "Location where it is mounted."), None, "mountPoint is mount and point");
        assert_eq!(words_in("services.openssh.settings PasswordAuthentication mountPoint x86_64"), ["services", "openssh", "settings", "password", "authentication", "mount", "point", "x86", "64"]);
    }
    #[test]
    fn words_split_like_a_shell_with_pipes() {
        assert_eq!(words("show | compare").unwrap(), ["show", "|", "compare"]);
        assert_eq!(words("show|display set").unwrap(), ["show", "|", "display", "set"]);
        assert_eq!(words(r#"commit comment "two words" "#).unwrap(), ["commit", "comment", "two words"]);
        assert_eq!(words(r#"set a "say \"hi\"""#).unwrap(), ["set", "a", "say \"hi\""]);
        assert_eq!(words(r#"set a """#).unwrap(), ["set", "a", ""]);
        assert!(words(r#"set "open"#).is_err());
    }

    #[test]
    fn a_quoted_bar_alone_is_never_a_pipe() {
        let e = format!("{:#}", words(r#"show "|" compare"#).unwrap_err());
        assert!(e.contains("cannot be a word"), "{e}");
        assert!(words(r#"commit comment "a | b""#).is_ok(), "inside other text it is text");
        assert_eq!(words("grep 'a \\d '\"\"").unwrap(), ["grep", "a \\d "], "single quotes: all literal");
        assert_eq!(words(r#"grep "\d\"\\""#).unwrap(), ["grep", r#"\d"\"#], "double quotes: only \" and \\ escape");
        assert_eq!(words(r#"commit comment "it's""#).unwrap(), ["commit", "comment", "it's"]);
        assert!(words("grep 'open").is_err());
        let e = format!("{:#}", words("commit comment it's fine").unwrap_err());
        assert!(e.contains("put the text in \"double quotes\""), "an apostrophe opens a quote, as in a shell: {e}");
    }

    /// A `flake.lock` whose `torq` input was last changed at `t` (seconds).
    fn lock(t: u64) -> String {
        format!(r#"{{"nodes":{{"root":{{"inputs":{{"torq":"torq"}}}},"torq":{{"locked":{{"lastModified":{t},"narHash":"h{t}"}}}}}},"version":7}}"#)
    }
    /// The software the fake machine finds newer (`request system update`), and with `all`.
    const NEWER: u64 = 1_793_400_000;
    const NEWER_ALL: u64 = 1_793_500_000;
    /// The test repo, then two commits of software: `OLD` (rollback 1), then `NOW` (rollback 0).
    const OLD: u64 = 1_790_000_000;
    const NOW: u64 = 1_793_000_000;
    fn repo_with_software() -> tempfile::TempDir {
        let d = repo();
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm old-software", lock(OLD)));
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git commit -qam software", lock(NOW)));
        d
    }

    #[test]
    fn request_system_update_loads_the_software_into_the_candidate() {
        let (d, m) = (repo_with_software(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let out = s.exec("request system update").unwrap();
        let (now, newer) = (crate::pkg::ymd(NOW), crate::pkg::ymd(NEWER));
        assert_eq!(
            out,
            format!("[edit system software]\n-   torq {now};\n+   torq {newer};\nLoaded into the candidate: `commit` (in configuration mode) installs it.\n")
        );
        s.exec("configure").unwrap();
        assert!(s.exec("show | compare").unwrap().starts_with(&format!("[edit system software]\n-   torq {now};")), "nothing else changed");
        let raw = s.exec("show | compare | display diff").unwrap();
        assert!(raw.contains("+++ b/flake.lock") && raw.contains(&format!("h{NEWER}")), "{raw}");
        s.exec("commit check").unwrap();
        assert_eq!(m.checks.borrow().last().unwrap(), &vec![("flake.lock".to_string(), Some(lock(NEWER)))], "checked with the new software");
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, reason, _) = m.commits.borrow()[0].clone();
        assert_eq!(diff, raw, "commit proposes what | display diff showed");
        assert_eq!(tier, "system", "software is the system's");
        assert!(reason.contains(&format!("+ system software torq {newer};")), "{reason}");
        assert!(m.asked.borrow()[0].contains("[edit system software]"), "a person sees it before it is committed");
        // `all`: every input.
        let out = s.exec("run request system update all").unwrap();
        assert!(out.contains(&format!("+   torq {};", crate::pkg::ymd(NEWER_ALL))) && out.ends_with("Loaded into the candidate: `commit` (in configuration mode) installs it.\n"), "{out}");
    }

    /// ADR-028: a machine set up from the image runs the TorqOS inside it; its first update moves it to
    /// the releases. From then on, an update takes only a commit the release key signed.
    #[test]
    fn updates_come_from_the_releases_signed() {
        let rev = "0123456789abcdef0123456789abcdef01234567";
        let image = "path:/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";
        let machine = |source: &str| {
            let d = repo_with_software();
            sh(d.path(), &format!("printf '{{\\n  inputs = {{\\n    torq.url = \"{source}\";\\n  }};\\n}}\\n' > flake.nix && git add -A && git commit -qm flake"));
            d
        };
        let releases = crate::software::RELEASES;

        // From the image: moved to the releases, the newest one checked, both in the candidate.
        let d = machine(image);
        let m = Fake { answer: true, release: Some((rev.into(), true)), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let out = s.exec("request system update").unwrap();
        assert!(out.starts_with(&format!("This machine runs the TorqOS it was installed with: the update moves it to TorqOS's releases ({releases}).\n[edit system software]\n")), "{out}");
        assert!(out.ends_with("TorqOS release 0123456789ab: signed by the TorqOS release key.\nThe new source is locked into the candidate: `commit` installs it.\n"), "{out}");
        assert_eq!(*m.release_checks.borrow(), [rev]);
        assert!(m.flakes.borrow()[0].contains(&format!("torq.url = \"{releases}\";")), "locked as it will be");
        s.exec("configure").unwrap();
        let compare = s.exec("show | compare").unwrap();
        assert!(compare.starts_with(&format!("[edit system software torq]\n-   source {image};\n+   source {releases};\n")), "{compare}");
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        assert!(m.commits.borrow()[0].0.contains(&format!("+    torq.url = \"{releases}\";")));

        // A commit the release key did not sign: refused, and nothing is loaded.
        let d = machine(image);
        let m = Fake { answer: true, release: Some((rev.into(), false)), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("request system update").unwrap_err());
        assert_eq!(e, "release 0123456789ab is not signed by the TorqOS release key");
        s.exec("configure").unwrap();
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.\n");

        // On the releases already: the update is of them, checked the same way.
        let d = machine(releases);
        let m = Fake { answer: true, release: Some((rev.into(), true)), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let out = s.exec("request system update").unwrap();
        assert!(out.ends_with("Loaded into the candidate: `commit` (in configuration mode) installs it.\nTorqOS release 0123456789ab: signed by the TorqOS release key.\n"), "{out}");
        let m = Fake { answer: true, release: Some((rev.into(), false)), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        assert!(format!("{:#}", s.exec("request system update").unwrap_err()).contains("not signed by the TorqOS release key"));
        s.exec("configure").unwrap();
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.\n");
        // Any other source: no releases, nothing to check.
        let d = machine("github:netstacks/torqos");
        let m = Fake { answer: true, ..Default::default() };
        let mut s = session(d.path(), None, &m);
        s.exec("request system update").unwrap();
        assert!(m.release_checks.borrow().is_empty());
    }

    #[test]
    fn up_to_date_software_loads_nothing() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm newest", lock(NEWER)));
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("request system update").unwrap(), "This machine has the newest software: nothing to load.\n");
        assert_eq!(s.exec("configure").unwrap(), "Entering configuration mode\n");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
");
    }

    #[test]
    fn software_rollback_loads_an_earlier_commits_software_as_a_downgrade() {
        let (d, m) = (repo_with_software(), Fake::default());
        let saved = d.path().join("saved.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        let out = s.exec("request system software rollback 1").unwrap();
        let (now, old) = (crate::pkg::ymd(NOW), crate::pkg::ymd(OLD));
        assert!(out.starts_with(&format!("[edit system software]\n-   torq {now};\n+   torq {old};\n## older software than now: a downgrade\n")), "{out}");
        assert_eq!(
            s.exec("request system software rollback 0").unwrap(),
            "Configuration 0 has this machine's software already.\nThe software loaded into the candidate before is dropped.\n",
            "going back to the base's software drops the downgrade, and says so"
        );
        let e = format!("{:#}", s.exec("request system software rollback 2").unwrap_err());
        assert!(e.contains("configuration 2 has no flake.lock"), "{e}");
        // Software that is not a flake.lock is never loaded.
        let (d2, m2) = (repo_with_software(), Fake::default());
        sh(d2.path(), "printf 'not json' > flake.lock && git commit -qam broken");
        let mut s2 = session(d2.path(), None, &m2);
        let e = format!("{:#}", s2.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("not JSON; nothing loaded"), "{e}");
        // Loading the same software again leaves nothing to commit; another session sees what is kept.
        s.exec("request system software rollback 1").unwrap();
        let mut again = session(d.path(), Some(saved.clone()), &m);
        again.exec("configure").unwrap();
        assert!(again.exec("show | compare").unwrap().contains(&format!("+   torq {old};")), "the candidate keeps its software");
        assert_eq!(again.exec("exit").unwrap(), "The candidate has changes not yet committed: they are kept (`configure` returns to them, `rollback 0` discards them).\n");
        again.exec("configure").unwrap();
        assert_eq!(again.exec("rollback 0").unwrap(), "Discarded 1 change: the candidate is the configuration in effect.\n");
        assert_eq!(again.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
", "rollback 0 discards the software too");
        // `rollback <n>` loads a configuration, never its software.
        again.exec("rollback 1").unwrap();
        assert!(!again.exec("show | compare").unwrap().contains("software"), "rollback n leaves the software alone");
    }

    #[test]
    fn the_software_is_loaded_against_the_candidates_base() {
        let (d, m) = (repo_with_software(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("configure").unwrap();
        s.exec("set system time-zone UTC").unwrap();
        let base = s.repo.head().unwrap();
        sh(d.path(), "printf '{ \"host-name\": \"other\" }\\n' > system/host.json && git commit -qam other");
        let out = s.exec("run request system update").unwrap();
        assert_eq!(m.bases.borrow().as_slice(), [base], "the candidate's base, not HEAD");
        assert!(out.contains("has changed since your candidate was started") && out.contains("`rollback 0` starts over"), "{out}");
    }

    #[test]
    fn rollback_n_keeps_the_loaded_software_rollback_0_drops_it() {
        let (d, m) = (repo_with_software(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("request system update").unwrap();
        s.exec("configure").unwrap();
        assert_eq!(s.exec("rollback 3").unwrap(), "Loaded configuration 3: 2 changes from the configuration in effect (`show | compare` lists them; `commit` makes it the machine's). The software loaded into the candidate is kept.\n");
        let c = s.exec("show | compare").unwrap();
        assert!(c.contains("hostName one;") && c.contains("[edit system software]"), "{c}");
        assert_eq!(s.exec("rollback 0").unwrap(), "Discarded 2 changes: the candidate is the configuration in effect.\n");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
");
    }

    #[test]
    fn the_software_comparison_shows_every_move_safely() {
        let node = |t: u64| format!(r#"{{"locked":{{"lastModified":{t},"narHash":"h{t}"}}}}"#);
        let lock = |nodes: &[(&str, String)]| {
            let n: Vec<String> = nodes.iter().map(|(k, v)| format!("{}:{v}", serde_json::to_string(k).unwrap())).collect();
            format!(r#"{{"nodes":{{"root":{{}},{}}},"version":7}}"#, n.join(","))
        };
        let (a, b) = (lock(&[("torq", node(NOW)), ("extra", node(OLD))]), lock(&[("torq", node(NOW - 10))]));
        let out = software_compare(Some(a.as_bytes()), &Some(b.clone()));
        assert!(out.contains("-   extra ") && !out.contains("+   extra"), "a removed input shows: {out}");
        assert!(out.contains("## older software than now: a downgrade"), "ten seconds older, in the same minute, is older: {out}");
        let evil = lock(&[("torq", node(NOW)), ("x;\n+   fine", node(NEWER))]);
        let out = software_compare(Some(b.as_bytes()), &Some(evil));
        assert!(!out.lines().any(|l| l.starts_with("+   fine")), "a name draws no line of its own: {out}");
    }

    #[test]
    fn loading_the_bases_software_says_it_drops_what_was_loaded() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git add -A && git commit -qm old", lock(OLD)));
        sh(d.path(), &format!("printf '%s' '{}' > flake.lock && git commit -qam newest", lock(NEWER)));
        let mut s = session(d.path(), None, &m);
        s.exec("request system software rollback 1").unwrap();
        assert_eq!(s.exec("request system update").unwrap(), "This machine has the newest software.\nThe software loaded into the candidate before is dropped.\n");
        assert_eq!(s.exec("configure").unwrap(), "Entering configuration mode\n");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
");
    }

    #[test]
    fn a_linked_flake_lock_is_never_followed() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{}' > real.lock && ln -s real.lock flake.lock && git add -A && git commit -qm link");
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("flake.lock at") && e.contains("is not a plain file"), "{e}");
    }

    #[test]
    fn a_long_reason_wraps_on_a_screen_not_into_a_filter() {
        let d = repo();
        let head = Repo::new(d.path()).head().unwrap();
        let long = "Install htop and jq for viper, and turn on the firewall";
        let m = Fake { ledger: Some(vec![Configuration { id: head, date: "d".into(), who: "viper".into(), what: long.into(), seq: Some(1) }]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let head = "Rollback  Change  When  By     What\n";
        assert_eq!(s.exec("show system commit").unwrap(), format!("{head}0         #1      d     viper  {long}\n"), "no screen (a script): one line");
        s.screen = || Some(60);
        assert_eq!(s.exec("show system commit").unwrap(), format!("{head}0         #1      d     viper  Install htop and jq for\n{0}viper, and turn on the\n{0}firewall\n", " ".repeat(31)), "wrapped under where it starts");
        s.screen = || Some(40);
        assert_eq!(s.exec("show system commit").unwrap(), format!("{head}0         #1      d     viper\n    Install htop and jq for viper, and\n    turn on the firewall\n"), "too narrow beside it: under it");
        #[cfg(target_os = "linux")] // the filters' sandbox
        {
            s.screen = || Some(44);
            assert_eq!(s.exec("show system commit | wc -l").unwrap(), "2\n", "a filter reads one line per commit (and the heading)");
        }
    }

    /// After a commit confirmed rolls back, the config repo is reset to the commit before it: git's
    /// history no longer has it, the ledger does. The numbers are the ledger's.
    /// `show | compare rollback <n>`: the candidate against an earlier configuration — its settings,
    /// and the config files it has from its own base, untouched.
    #[test]
    fn compare_with_an_earlier_configuration() {
        let d = repo();
        sh(d.path(), "printf '{ ... }:\\n{\\n}\\n' > system.nix && git add -A && git commit -qm third");
        let ids: Vec<String> = Repo::new(d.path()).history(3).unwrap().into_iter().map(|c| c.id).collect();
        let conf = |id: &str, what: &str, seq| Configuration { id: id.into(), date: "d".into(), who: "viper".into(), what: what.into(), seq: Some(seq) };
        let m = Fake { ledger: Some(vec![conf(&ids[0], "third", 3), conf(&ids[1], "second", 2), conf(&ids[2], "first", 1)]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure"]);
        assert_eq!(s.exec("show | compare rollback 0").unwrap(), "No changes: the candidate is configuration 0 (rollback 0).\n");
        let one = s.exec("show | compare rollback 1").unwrap();
        assert!(one.contains("[edit config files]") && one.contains("+++ b/system.nix") && !one.contains("hostName"), "the file it has from its base: {one}");
        let two = s.exec("show | compare rollback 2").unwrap();
        assert!(two.contains("-   hostName one;") && two.contains("+   hostName two;"), "{two}");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.\n", "against its own base, as before");
        assert!(s.exec("show | compare rollback 9").unwrap_err().to_string().contains("9"), "a configuration it does not have");
    }

    #[test]
    fn rollback_numbers_are_the_ledgers() {
        let d = repo();
        let ids: Vec<String> = Repo::new(d.path()).history(2).unwrap().into_iter().map(|c| c.id).collect();
        let (two, one) = (ids[0].clone(), ids[1].clone());
        sh(d.path(), "git reset -q --hard HEAD~1");
        let conf = |id: &str, who: &str, what: &str, seq| Configuration { id: id.into(), date: "d".into(), who: who.into(), what: what.into(), seq: Some(seq) };
        let m = Fake {
            ledger: Some(vec![conf(&one, "broker", "Change #2 rolled back", 3), conf(&two, "viper", "host two", 2), conf(&one, "user", "init", 1)]),
            ..Default::default()
        };
        let mut s = session(d.path(), None, &m);
        let commits = s.exec("show system commit").unwrap();
        assert_eq!(commits, "Rollback  Change  When  By      What\n0         #3      d     broker  Change #2 rolled back\n1         #2      d     viper   host two\n2         #1      d     user    init\n", "rollback number, change number");
        assert!(s.exec("show system commit | display detail").unwrap().contains(&format!("    commit {two}\n")));
        assert_eq!(s.exec("request system revert #2").unwrap(), s.exec("request system revert 2").unwrap(), "a change number as the ledger writes it");
        assert_eq!(s.exec("show system rollback 1").unwrap(), "networking {\n    hostName two;\n}\n", "git's history alone has no 1");
        s.exec("configure").unwrap();
        s.exec("rollback 1").unwrap();
        assert!(s.exec("show | compare").unwrap().contains("+   hostName two;"));
        let e = format!("{:#}", s.exec("rollback 3").unwrap_err());
        assert_eq!(e, "there is no configuration 3: there are 3 (0 to 2; `show system commit` lists them)", "the line is named once, by the shell");
    }

    #[test]
    fn a_repo_away_from_the_ledger_is_said() {
        let d = repo();
        let first = Repo::new(d.path()).history(2).unwrap()[1].id.clone();
        let m = Fake { ledger: Some(vec![Configuration { id: first, date: "d".into(), who: "u".into(), what: "w".into(), seq: Some(1) }]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show system commit").unwrap();
        assert!(out.ends_with("not where the ledger says: `show system alarms`\n"), "{out}");
        s.exec("configure").unwrap();
        s.exec("set system host-name x").unwrap();
        s.exec("rollback 0").unwrap();
        assert_eq!(s.exec("show").unwrap(), "networking {\n    hostName two;\n}\n", "rollback 0: what is active (HEAD)");
        // 0 is what is active for every command, not the ledger's newest.
        assert_eq!(s.exec("run show system rollback 0").unwrap(), "networking {\n    hostName two;\n}\n");
        let e = format!("{:#}", s.exec("run request system software rollback 0").unwrap_err());
        assert!(e.contains("configuration 0 has no flake.lock"), "HEAD's, not the ledger's: {e}");
    }

    #[test]
    fn the_drift_note_counts_as_the_broker_does() {
        // An interrupted change (which numbers nothing) moved the repo: the broker says all is well.
        let d = repo();
        let ids: Vec<String> = Repo::new(d.path()).history(2).unwrap().into_iter().map(|c| c.id).collect();
        let conf = |id: &str| Configuration { id: id.into(), date: "d".into(), who: "u".into(), what: "w".into(), seq: Some(1) };
        let m = Fake { ledger: Some(vec![conf(&ids[1])]), ledger_at: Some(ids[0].clone()), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        assert!(!s.exec("show system commit").unwrap().contains("##"), "no drift");
        // A configuration whose commit is gone says so, not git's error.
        let missing = "0123456789abcdef0123456789abcdef01234567";
        let m = Fake { ledger: Some(vec![conf(&ids[0]), conf(missing)]), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("show system rollback 1").unwrap_err());
        assert!(e.contains("configuration 1 (commit 0123456789ab) is no longer in the config repo"), "{e}");
    }

    #[test]
    fn the_ledger_and_reverting_one_change() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show system ledger").unwrap(), "ledger 20 fitting None\n", "no window");
        s.screen = || Some(50);
        assert_eq!(s.exec("show system ledger").unwrap(), "ledger 20 fitting Some(50)\n", "fitting the window");
        #[cfg(target_os = "linux")] // the filters' sandbox
        assert_eq!(s.exec("show system ledger | head").unwrap(), "ledger 20 fitting None\n", "a filter reads whole rows, as before");
        s.screen = || None;
        assert_eq!(s.exec("show system ledger 5 | wc -l").unwrap(), "1\n");
        assert!(format!("{:#}", s.exec("show system ledger 0").unwrap_err()).contains("1 to 10000"));
        assert_eq!(s.exec("request system revert 7").unwrap(), "reverted 7 for dev\n");
        assert!(format!("{:#}", s.exec("request system revert x").unwrap_err()).contains("'x' is not a number (change numbers: 1, 2, …)"));
        s.exec("configure").unwrap();
        assert_eq!(s.exec("run request system revert 3").unwrap(), "reverted 3 for dev\n");
    }

    #[test]
    fn turning_ssh_on_shows_the_keys_it_admits_and_needs_one() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set services openssh enable true"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("lets in dev's keys only, and they have none") && e.contains("PasswordAuthentication true"), "{e}");
        assert!(m.commits.borrow().is_empty() && m.asked.borrow().is_empty(), "nothing asked, nothing committed");
        assert_eq!(m.ssh_asked.borrow()[0][0].0, "nixos/services.json", "as the change will have it");
        // The old list is never added to: it says what to set instead.
        let e = format!("{:#}", s.exec("set torq services enabled ssh").unwrap_err());
        assert!(e.contains("old list of services: `set services openssh enable true` turns it on"), "{e}");

        let m = Fake { answer: true, keys: vec!["    256 SHA256:abc dev@mac (ED25519)  (from /home/dev/.ssh/authorized_keys)".into()], ..Default::default() };
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set services openssh enable true"]);
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let q = m.asked.borrow()[0].clone();
        assert!(q.contains("Keys that will be able to log in as dev:\n    256 SHA256:abc dev@mac (ED25519)  (from /home/dev/.ssh/authorized_keys)\n"), "{q}");
    }

    /// SSH already on: a change to who can log in is checked as turning it on is (audit of
    /// 2026-11-01) — the last key removed, passwords turned off with no key: refused; other keys:
    /// shown; nothing about SSH changed: nothing asked.
    #[test]
    fn a_change_to_who_can_log_in_is_checked_with_ssh_already_on() {
        let keys_only = Ssh { on: true, passwords: false };
        let with_passwords = Ssh { on: true, passwords: true };
        let key = "    256 SHA256:abc dev@mac (ED25519)  (from /home/dev/.ssh/authorized_keys)".to_string();
        let commit = |m: &Fake| {
            let d = repo();
            let mut s = session(d.path(), None, m);
            run(&mut s, &["configure", "set system host-name three"]);
            s.exec("commit").map_err(|e| format!("{e:#}"))
        };
        // The last key removed, SSH keys only.
        let m = Fake { answer: true, ssh_state: Some((keys_only, keys_only)), keys_before: Some(vec![key.clone()]), ..Default::default() };
        let e = commit(&m).unwrap_err();
        assert!(e.contains("not committed: SSH here would let in dev's keys only, and they have none"), "{e}");
        assert!(m.commits.borrow().is_empty());
        // Passwords turned off, no key.
        let m = Fake { answer: true, ssh_state: Some((with_passwords, keys_only)), ..Default::default() };
        assert!(commit(&m).unwrap_err().contains("they have none"));
        // Passwords turned off, with a key: the keys are approved as they are.
        let m = Fake { answer: true, ssh_state: Some((with_passwords, keys_only)), keys: vec![key.clone()], ..Default::default() };
        commit(&m).unwrap();
        assert!(m.asked.borrow()[0].contains("Keys that will be able to log in as dev:"), "{}", m.asked.borrow()[0]);
        // Other keys than before: shown.
        let m = Fake { answer: true, ssh_state: Some((keys_only, keys_only)), keys: vec![key.clone()], keys_before: Some(vec![]), ..Default::default() };
        commit(&m).unwrap();
        assert!(m.asked.borrow()[0].contains("Keys that will be able to log in as dev:"));
        // The same keys: nothing to say. Passwords newly allowed: said.
        let m = Fake { answer: true, ssh_state: Some((keys_only, keys_only)), keys: vec![key.clone()], ..Default::default() };
        commit(&m).unwrap();
        assert!(!m.asked.borrow()[0].contains("Keys that"), "{}", m.asked.borrow()[0]);
        let m = Fake { answer: true, ssh_state: Some((keys_only, with_passwords)), keys: vec![key], ..Default::default() };
        commit(&m).unwrap();
        assert!(m.asked.borrow()[0].contains("! SSH will accept passwords"), "{}", m.asked.borrow()[0]);
    }

    #[test]
    fn ssh_already_on_asks_no_keys() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        sh(d.path(), "printf '[\\n  \"ssh\"\\n]\\n' > services.json && git add -A && git commit -qm ssh");
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        s.exec("commit").unwrap();
        assert!(m.ssh_asked.borrow().is_empty());
    }

    #[test]
    fn show_services_lists_what_the_configuration_turns_on() {
        let (d, m) = (repo(), Fake::default());
        assert!(session(d.path(), None, &m).exec("show services").unwrap().starts_with("No services are turned on"));
        sh(d.path(), "mkdir -p nixos && printf '{\\n  \"openssh\": { \"enable\": true },\\n  \"printing\": { \"enable\": false }\\n}\\n' > nixos/services.json && printf '[\\n  \"podman\"\\n]\\n' > services.json && git add -A && git commit -qm on");
        let out = session(d.path(), None, &m).exec("show services").unwrap();
        assert!(out.starts_with("Services the configuration turns on:\n"), "{out}");
        assert!(out.contains("  services openssh") && out.contains("SSH: keys only; root never logs in"), "{out}");
        assert!(out.contains("  virtualisation podman") && !out.contains("printing"), "the old list's too; only what is on: {out}");
        assert!(out.contains("! Some come from the old list of services (services.json). To move them: `set virtualisation podman enable true`"), "{out}");
    }

    /// The whole device: what it runs, and what is set outside the tree, as comments a file keeps.
    #[test]
    fn show_configuration_names_the_whole_device() {
        let (d, m) = (repo(), Fake::default());
        let lock = r#"{"nodes":{"root":{"inputs":{"torq":"torq","nixpkgs":["torq","nixpkgs"]}},"torq":{"locked":{"lastModified":1793000000}},"nixpkgs":{"locked":{"lastModified":1790000000}}},"version":7}"#;
        sh(d.path(), &format!("printf '%s' '{lock}' > flake.lock && printf '{{ }}\\n' > machine.nix && printf '{{ }}\\n' > flake.nix && git add -A && git commit -qm device"));
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show configuration").unwrap();
        assert_eq!(
            out,
            "networking {\n    hostName two;\n}\n## software: torq 2026-10-26\n## also set outside this configuration, in Nix: machine.nix\n",
            "the machine's own inputs only (nixpkgs follows torq\x27s)"
        );
        assert!(!s.exec("show configuration system").unwrap().contains("##"), "a part of it is just that part");
        assert!(s.exec("show configuration | display set").unwrap().ends_with("in Nix: machine.nix\n"));
        // The candidate names the software it would install.
        s.exec("request system update").unwrap();
        s.exec("configure").unwrap();
        assert!(s.exec("show").unwrap().contains(&format!("## software: torq {}\n", crate::pkg::ymd(NEWER))));
        // And what it shows reads back: comments are not commands.
        let saved = s.exec("run show configuration | display set").unwrap();
        for line in saved.lines() {
            s.exec(line).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        }
    }

    /// A NixOS setting commits as its nixos/ file, system tier, asked; a person's own packages,
    /// by their NixOS path, stay user tier.
    #[test]
    fn nixos_settings_commit_as_their_files_at_their_tier() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set virtualisation docker enable true", "set networking firewall allowedTCPPorts [ 80 443 ]"]);
        assert_eq!(s.exec("show | compare").unwrap(), "[edit networking firewall]\n+   allowedTCPPorts [ 80 443 ];\n[edit virtualisation docker]\n+   enable true;\n");
        s.exec("commit check").unwrap();
        let checked: Vec<String> = m.checks.borrow()[0].iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(checked, ["nixos/networking.json", "nixos/virtualisation.json"], "checked as the files it writes");
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, reason, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/nixos/virtualisation.json") && diff.contains("\"enable\": true"), "{diff}");
        assert_eq!(tier, "system");
        assert_eq!(reason, "+ networking firewall allowedTCPPorts [ 80 443 ]; + virtualisation docker enable true;");
        assert!(m.asked.borrow()[0].contains("This changes the system (all users)."));
        // Your own packages, by their NixOS path: your folder, no question.
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users users dev packages [ htop ]"]);
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        assert_eq!(m.commits.borrow()[0].2, "user");
        assert!(m.commits.borrow()[0].0.contains("+++ b/users/dev/packages.json"));
        assert!(m.asked.borrow().is_empty());
    }

    /// Config files: attached in the tree, edited in the candidate, shown as their raw diff, committed
    /// as the files they are (system tier, asked), kept across sessions; SSH counted however it is set.
    /// A file that is not Nix as written: edited again when the person wants, else kept as it is
    /// (nothing typed lost), saying so.
    #[test]
    fn a_file_not_nix_is_edited_again_or_kept_saying_so() {
        let broken = "{ ... }:\n{\n  services.httpd.enable = true;\n";
        let fixed = "{ ... }:\n{\n  services.httpd.enable = true;\n}\n";
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some(broken.into()), edit_again: Some(fixed.into()), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure"]);
        assert!(s.exec("file edit a.nix").unwrap().starts_with("a.nix: changed in the candidate (not attached"));
        assert_eq!(m.edited.borrow()[1], ("a.nix".into(), broken.into()), "edited again from what was typed");
        assert!(m.asked.borrow()[0].starts_with("a.nix is not Nix as written: unexpected end of file, at a.nix:3:1\nEdit it again?"), "{:?}", m.asked.borrow());
        assert_eq!(s.exec("file show a.nix").unwrap(), fixed);
        let (d, m) = (repo(), Fake { no: true, edit_to: Some(broken.into()), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure"]);
        assert_eq!(s.exec("file edit a.nix").unwrap(), "a.nix: changed in the candidate, but not Nix yet (unexpected end of file, at a.nix:3:1): `file edit a.nix` to fix it; `commit` refuses it until then\n");
        assert_eq!(s.exec("file show a.nix").unwrap(), broken, "kept as typed");
        assert_eq!(s.exec("rollback 0").unwrap(), "Discarded 1 change: the candidate is the configuration in effect.\n", "a file is one change, however many lines");
        // A sentence a command answers with fits the window (said): not cut mid-word by the terminal.
        s.screen = || Some(40);
        let said = s.exec("rollback 0").unwrap();
        assert!(said.lines().count() > 1 && said.lines().all(|l| l.chars().count() <= 40), "{said}");
    }

    #[test]
    fn config_files_are_part_of_the_configuration() {
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }:\n{\n  services.httpd.enable = true;\n}\n".into()), ..Default::default() });
        let saved = d.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        run(&mut s, &["configure", "file edit services/httpd.nix"]);
        assert_eq!(m.edited.borrow()[0], ("services/httpd.nix".into(), new_nix_file("services/httpd.nix")), "a new file starts as an empty module, saying what it is");
        assert!(new_nix_file("services/httpd.nix").contains("set <place> config services/httpd.nix") && new_nix_file("x.nix").ends_with("{ config, lib, pkgs, ... }:\n{\n}\n"));
        run(&mut s, &["set services httpd config services/httpd.nix"]);
        assert_eq!(s.exec("show services httpd").unwrap(), "config services/httpd.nix;\n");
        assert_eq!(s.exec("show services | display set").unwrap(), "set services httpd config services/httpd.nix\n", "whole lines, to paste back");
        run(&mut s, &["edit services"]);
        assert_eq!(s.exec("show | display set").unwrap(), "set services httpd config services/httpd.nix\n", "from where you are, too");
        run(&mut s, &["top"]);
        // Another session (the candidate as saved) sees the same.
        let mut s2 = session(d.path(), Some(saved), &m);
        s2.exec("configure").unwrap();
        assert_eq!(s2.exec("file show services/httpd.nix").unwrap(), "{ ... }:\n{\n  services.httpd.enable = true;\n}\n");
        assert_eq!(s2.exec("file list").unwrap(), "services/httpd.nix (new in the candidate)  [attached at: services httpd]\n");
        let compare = s.exec("show | compare").unwrap();
        assert!(compare.starts_with("[edit services httpd]\n+   config services/httpd.nix;\n[edit config files]\n"), "{compare}");
        assert!(compare.contains("+++ b/services/httpd.nix") && compare.contains("+  services.httpd.enable = true;"), "{compare}");
        // Deleting an attached file is refused until it is not attached.
        assert!(s.exec("file delete services/httpd.nix").unwrap_err().to_string().contains("`delete services httpd config services/httpd.nix` first"));
        assert!(s.exec("file edit ../x.nix").is_err() && s.exec("set services httpd config /etc/x.nix").is_err());
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, _, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/config-files.json") && diff.contains("+  \"services httpd\": [\n+    \"services/httpd.nix\"\n+  ]"), "{diff}");
        assert!(diff.contains("+++ b/services/httpd.nix"), "{diff}");
        assert_eq!(tier, "system");
        assert!(m.asked.borrow()[0].contains("services.httpd.enable = true"), "the person approves the Nix as written");
        assert_eq!(*m.ssh_calls.borrow(), 1, "a config file may turn SSH on: SSH is evaluated");
        // Only a Nix file changed: SSH is still evaluated.
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }: { }\n".into()), ..Default::default() });
        sh(d.path(), "printf '{ ... }:\\n{\\n}\\n' > system.nix && git add -A && git commit -qm nix");
        let saved = d.path().join("c2.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        run(&mut s, &["configure", "file edit system.nix"]);
        // Kept as it is edited: another session sees it, with nothing else done.
        let mut s2 = session(d.path(), Some(saved), &m);
        assert_eq!(run(&mut s2, &["configure", "file list"]).lines().last(), Some("system.nix (edited in the candidate)"));
        run(&mut s, &["commit"]);
        assert_eq!(*m.ssh_calls.borrow(), 1);
        let (diff, _, _, reason, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("+++ b/system.nix"));
        assert_eq!(reason, "~ config file system.nix", "the ledger says which file");
        // A deletion is kept too, and said as one.
        let saved = d.path().join("c3.json");
        run(&mut session(d.path(), Some(saved.clone()), &m), &["configure", "file delete system.nix"]);
        assert_eq!(run(&mut session(d.path(), Some(saved), &m), &["configure", "file list"]).lines().last(), Some("system.nix (deleted in the candidate)"));
        // Unattaching: `delete <place> config`; deleting the place takes it too.
        let mut d2 = crate::whole::Data::default();
        d2.attached.insert("services httpd".into(), ["a.nix".to_string()].into());
        let x = s.whole();
        assert_eq!(x.delete(&mut d2.clone(), &w("services httpd config")).unwrap(), crate::whole::Deleted::Done);
        let mut d3 = d2.clone();
        assert_eq!(x.delete(&mut d3, &w("services httpd")).unwrap(), crate::whole::Deleted::Done);
        assert!(d3.attached.is_empty());
        assert_eq!(x.delete(&mut crate::whole::Data::default(), &w("services httpd config")).unwrap(), crate::whole::Deleted::NotFound);
        // Below named entries, `config` is no name (a reserved word), and no place to attach.
        let mut d4 = crate::whole::Data::default();
        assert!(x.set(&mut d4, &w("services httpd virtualHosts config documentRoot /srv")).unwrap_err().to_string().contains("keeps the word"));
        assert!(x.set(&mut d4, &w("services httpd virtualHosts config x.nix")).is_err() && d4 == Default::default(), "not attached there either");
    }

    /// In operational mode, the active configuration's files; editing is configuration mode's.
    #[test]
    fn file_outside_configuration_mode_only_reads() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{ ... }: { }\\n' > system.nix && git add -A && git commit -qm nix");
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("file show system.nix").unwrap(), "{ ... }: { }\n");
        assert_eq!(s.exec("file list").unwrap(), "system.nix\n");
        assert!(s.exec("file edit system.nix").unwrap_err().to_string().contains("a configuration-mode command (after `file`): `configure` first"));
        assert!(s.exec("file show flake.nix").is_err(), "the repo's own structure is not a config file");
        assert!(s.exec("show configuration").unwrap().contains("in Nix: system.nix"));
        // Once attached, it is part of the configuration: shown there, not as outside it.
        sh(d.path(), "printf '{ \"services httpd\": \"system.nix\" }\\n' > config-files.json && git add -A && git commit -qm attach");
        let shown = s.exec("show configuration").unwrap();
        assert!(shown.contains("httpd {\n        config system.nix;") && !shown.contains("in Nix"), "{shown}");
        sh(d.path(), "touch flake.nix && git add -A && git commit -qm flake");
        assert!(!s.exec("show configuration").unwrap().contains("in Nix"), "flake.nix is TorqOS's own, not set outside");
        assert_eq!(s.exec("file list").unwrap(), "flake.nix  (TorqOS's own: it builds the machine from the tree)\nsystem.nix  [attached at: services httpd]\n");
        // At the top of the tree, and TorqOS's own flake.nix said as such.
        sh(d.path(), "printf '{ \"\": \"system.nix\" }\\n' > config-files.json && touch flake.nix && git add -A && git commit -qm top");
        assert!(s.exec("show configuration").unwrap().starts_with("config system.nix;\n"));
        assert_eq!(s.exec("file list").unwrap(), "flake.nix  (TorqOS's own: it builds the machine from the tree)\nsystem.nix  [attached at: the top]\n");
        let mut d2 = crate::whole::Data::default();
        s.whole().set(&mut d2, &w("config a.nix")).unwrap();
        assert_eq!(crate::attach::load(crate::attach::render(&d2.attached).as_deref().map(str::as_bytes)).unwrap(), d2.attached, "a top-level attachment reads back");
    }

    /// The layout (ui.rs), through real commands at every window size: each sentence fits the window,
    /// wrapped under where its text starts; the comparison is exactly as it is; without a window,
    /// nothing is wrapped. A window over 80 columns is laid out to 80.
    #[test]
    fn every_window_size() {
        let fits = |text: &str, fit: usize| text.lines().all(|l| crate::ui::width(l) <= fit);
        for (screen, fit) in [(None, None), (Some(12), Some(20)), (Some(40), Some(40)), (Some(80), Some(80)), (Some(200), Some(80))] {
            let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
            let mut s = session(d.path(), None, &m);
            s.screen = match screen {
                None => || None,
                Some(12) => || Some(12),
                Some(40) => || Some(40),
                Some(80) => || Some(80),
                _ => || Some(200),
            };
            run(&mut s, &["configure", "set boot loader timeout 3"]);
            let compare = s.exec("show | compare").unwrap();
            run(&mut s, &["commit"]);
            let asked = m.asked.borrow()[0].clone();
            assert!(asked.starts_with(&compare), "{fit:?}: the comparison as it is:\n{asked}");
            let after = &asked[compare.len()..];
            assert!(after.starts_with("! This changes how"), "{fit:?}: {after}");
            assert!(after.ends_with("Commit? "), "{fit:?}: the answer goes on the same line: {after}");
            match fit {
                None => assert_eq!(after.lines().count(), 2, "no window: one line each: {after}"),
                Some(f) => {
                    assert!(fits(after, f), "{f}: {after}");
                    assert!(after.lines().nth(1).is_some_and(|l| l.starts_with("  ") && !l.starts_with("   ")), "{f}: under its text: {after}");
                }
            }
        }
    }

    /// A change to how the machine starts is said so before anyone approves it; others are not.
    #[test]
    fn boot_changes_are_warned_about() {
        for (line, warned) in [
            ("set fileSystems /boot fsType vfat", true),
            ("set boot loader timeout 3", true),
            ("set virtualisation docker enable true", false),
            ("set environment variables BOOT x", false),
        ] {
            let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
            let mut s = session(d.path(), None, &m);
            run(&mut s, &["configure", line, "commit"]);
            let asked = m.asked.borrow()[0].clone();
            assert_eq!(asked.contains("how the machine starts"), warned, "{line}: {asked}");
        }
        // Editing the attached hardware file is warned about too.
        let (d, m) = (repo(), Fake { answer: true, edit_to: Some("{ ... }: { }\n".into()), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "file edit hardware-configuration.nix", "commit"]);
        assert!(m.asked.borrow()[0].contains("how the machine starts"));
    }

    /// `system software torq source`: shown from flake.nix, set (locked at once, into the candidate),
    /// committed as flake.nix's one line and the new lock together; never deleted; kept across
    /// sessions; and refused for a flake.nix that is not TorqOS's fixed one.
    #[test]
    fn the_software_source_is_a_setting() {
        let (d, m) = (repo_with_software(), Fake { answer: true, ..Default::default() });
        sh(d.path(), "printf '{\\n  inputs = {\\n    torq.url = \"github:netstacks/torqos\";\\n  };\\n}\\n' > flake.nix && mkdir -p nixos && printf '{ \"dev\": { \"enable\": true } }\\n' > nixos/torq.json && git add -A && git commit -qm flake");
        let saved = d.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        assert!(s.exec("show configuration system software").unwrap().contains("source github:netstacks/torqos;"));
        s.exec("configure").unwrap();
        let out = s.exec("set system software torq source path:/home/dev/torq").unwrap();
        assert!(out.contains("[edit system software]") && out.contains("locked into the candidate"), "{out}");
        assert!(m.flakes.borrow()[0].contains("torq.url = \"path:/home/dev/torq\";"), "locked with the new flake.nix");
        // Kept: another session (the candidate as saved) has it.
        let mut s2 = session(d.path(), Some(saved), &m);
        s2.exec("configure").unwrap();
        let compare = s2.exec("show | compare").unwrap();
        assert!(compare.starts_with("[edit system software torq]\n-   source github:netstacks/torqos;\n+   source path:/home/dev/torq;\n"), "{compare}");
        assert!(compare.contains("[edit system software]\n"), "and the new lock: {compare}");
        // Not deleted; not anything else; only plain sources.
        assert!(s.exec("delete system software torq source").unwrap_err().to_string().contains("not deleted"));
        assert!(s.exec("set system software torq source file:///etc").is_err());
        assert!(s.exec("set system software torq version 1").is_err());
        // An update is of the new source; an earlier configuration's software is not loaded onto it.
        s.exec("run request system update").unwrap();
        assert_eq!(m.flakes.borrow().len(), 2);
        assert!(m.flakes.borrow()[1].contains("path:/home/dev/torq"), "updated as it will be");
        assert!(s.exec("run request system software rollback 1").unwrap_err().to_string().contains("another source"));
        assert_eq!(s.exec("commit").unwrap(), "commit complete (change #42)\n");
        let (diff, _, tier, reason, _) = m.commits.borrow()[0].clone();
        assert!(diff.contains("-    torq.url = \"github:netstacks/torqos\";\n+    torq.url = \"path:/home/dev/torq\";"), "{diff}");
        assert!(diff.contains("+++ b/flake.lock"), "with its lock: {diff}");
        assert_eq!(tier, "system");
        assert!(reason.starts_with("- system software torq source github:netstacks/torqos; + system software torq source path:/home/dev/torq;"), "{reason}");
        // A flake.nix that is not TorqOS's fixed one: no source shown, none set.
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{ }\\n' > flake.nix && git add -A && git commit -qm flake");
        let mut s = session(d.path(), None, &m);
        assert!(!s.exec("show configuration").unwrap().contains("software {"));
        assert!(run(&mut s, &["configure"]).contains("configuration mode"));
        assert!(s.exec("set system software torq source github:a/b").unwrap_err().to_string().contains("not TorqOS's fixed one"));
    }

    /// From the review of S4b: a lock made for a source `rollback <n>` does not keep is dropped with it
    /// (a lock always belongs to the flake.nix it is committed with); a folder as the source is said.
    #[test]
    fn a_lock_never_outlives_its_source() {
        let (d, m) = (repo_with_software(), Fake { answer: true, ..Default::default() });
        sh(d.path(), "printf '{\\n  inputs = {\\n    torq.url = \"github:netstacks/torqos\";\\n  };\\n}\\n' > flake.nix && mkdir -p nixos && printf '{ \"dev\": { \"enable\": true } }\\n' > nixos/torq.json && git add -A && git commit -qm flake");
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system software torq source github:other/torq"]);
        assert_eq!(s.exec("rollback 1").unwrap(), "Loaded configuration 1: 1 change from the configuration in effect (`show | compare` lists them; `commit` makes it the machine's).\n", "not kept: it was for the other source");
        assert!(!s.exec("show | compare").unwrap().contains("[edit system software"), "{}", s.exec("show | compare").unwrap());
        // Software loaded for the machine's own source is kept, as before.
        run(&mut s, &["rollback 0", "run request system update"]);
        assert!(s.exec("rollback 1").unwrap().contains("is kept"));
        // A folder: said in the question.
        run(&mut s, &["rollback 0", "set system software torq source path:/home/dev/torq", "commit"]);
        assert!(m.asked.borrow()[0].contains("comes from a folder"), "{}", m.asked.borrow()[0]);
    }

    /// `save` / `load`: a saved configuration recreates the candidate — the tree, the config files'
    /// content, the software's source — and loads all or nothing.
    #[test]
    fn a_saved_configuration_recreates_the_device() {
        let (d, m) = (repo_with_software(), Fake { answer: true, edit_to: Some("{ ... }:\n{\n  networking.domain = \"x\";\n}\n".into()), ..Default::default() });
        sh(d.path(), "printf '{\\n  inputs = {\\n    torq.url = \"github:netstacks/torqos\";\\n  };\\n}\\n' > flake.nix && git add -A && git commit -qm flake");
        let f = d.path().join("saved.conf");
        let file = f.to_str().unwrap();
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set virtualisation docker enable true", "set networking firewall allowedTCPPorts [ 80 443 ]", "file edit custom/a.nix", "set config custom/a.nix", "set environment variables GREETING \"hello there; {x}\""]);
        let wanted = s.exec("show | compare").unwrap();
        let out = s.exec(&format!("save {file}")).unwrap();
        assert!(out.starts_with("Wrote ") && out.contains(file), "{out}");
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&f).unwrap().permissions()) & 0o777, 0o600, "private");
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("## config file custom/a.nix\n##| { ... }:\n") && text.contains("source github:netstacks/torqos;"), "{text}");
        // Override: the file is the candidate — the same changes, from a fresh one.
        run(&mut s, &["rollback 0", "set time timeZone UTC"]);
        assert_eq!(s.exec(&format!("load override {file}")).unwrap(), "load complete\n");
        assert_eq!(s.exec("show | compare").unwrap(), wanted, "recreated exactly (and the time zone set before is gone)");
        // Merge: onto the candidate, which keeps its own.
        run(&mut s, &["rollback 0", "set time timeZone UTC", &format!("load merge {file}")]);
        let merged = s.exec("show | compare").unwrap();
        assert!(merged.contains("+   timeZone UTC;") && merged.contains("+   enable true;"), "{merged}");
        // Set: `| display set` lines, from the top, wherever configuration mode is.
        let lines = d.path().join("lines.set");
        run(&mut s, &["rollback 0", "set virtualisation docker enable true"]);
        std::fs::write(&lines, s.exec("show virtualisation | display set").unwrap() + "## a comment\n\ndelete virtualisation docker enable\nset time timeZone UTC\n").unwrap();
        run(&mut s, &["rollback 0", "edit networking", &format!("load set {}", lines.display()), "top"]);
        assert_eq!(s.exec("show | compare").unwrap(), "[edit time]\n+   timeZone UTC;\n", "the delete took the set before it");
        // All or nothing: one refused statement, and the candidate is as it was.
        std::fs::write(&lines, "set time timeZone UTC\nset torq broker rebuildCommand x\n").unwrap();
        run(&mut s, &["rollback 0", "set virtualisation docker enable true"]);
        let before = s.exec("show | compare").unwrap();
        let e = format!("{:#}", s.exec(&format!("load set {}", lines.display())).unwrap_err());
        assert!(e.contains("nothing loaded") && e.contains("line 2"), "{e}");
        assert_eq!(s.exec("show | compare").unwrap(), before);
        std::fs::write(&f, "virtualisation {\n    docker {\n        enable true;\n    }\n}\ntorq {\n    broker {\n        rebuildCommand x;\n    }\n}\n").unwrap();
        let e = format!("{:#}", s.exec(&format!("load override {file}")).unwrap_err());
        assert!(e.contains("nothing loaded") && e.contains("set torq broker rebuildCommand x"), "{e}");
        assert_eq!(s.exec("show | compare").unwrap(), before, "nothing of it");
        std::fs::write(&f, "virtualisation {\n    docker {\n").unwrap();
        assert!(format!("{:#}", s.exec(&format!("load merge {file}")).unwrap_err()).contains("line 2"));
        // Only `set` and `delete` lines: a file cannot commit, or run anything else.
        std::fs::write(&lines, "set time timeZone UTC\ncommit\n").unwrap();
        let e = format!("{:#}", s.exec(&format!("load set {}", lines.display())).unwrap_err());
        assert!(e.contains("line 2") && e.contains("only `set` and `delete`"), "{e}");
        assert!(m.commits.borrow().is_empty(), "nothing committed");
        assert!(s.exec("load sideways x").is_err() && s.exec("save").is_err());
    }

    /// From the review of S5: every Nix file is saved (one may import another); `| display set`
    /// always loads back (a `|`, an empty list); save writes a new private file and never through a
    /// link or over a device; load refuses a huge file, and an attachment with no content.
    #[test]
    fn save_and_load_keep_everything_and_write_safely() {
        let (d, m) = (repo_with_software(), Fake::default());
        sh(d.path(), "mkdir -p custom && printf '{ ... }: { imports = [ ./helper.nix ]; }\\n' > custom/a.nix && printf '{ ... }: { }\\n' > custom/helper.nix && printf '{ \"\": \"custom/a.nix\" }\\n' > config-files.json && git add -A && git commit -qm nix");
        let f = d.path().join("saved.conf");
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set environment variables PIPE \"a|b\"", "set networking firewall allowedTCPPorts [ ]", &format!("save {}", f.display())]);
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("## config file custom/helper.nix\n"), "not attached, still saved: {text}");
        // display set loads back exactly.
        let lines = d.path().join("l.set");
        std::fs::write(&lines, s.exec("show | display set").unwrap()).unwrap();
        let wanted = s.exec("show | compare").unwrap();
        run(&mut s, &["rollback 0", &format!("load set {}", lines.display())]);
        assert_eq!(s.exec("show | compare").unwrap(), wanted);
        // A link at the path is replaced, not written through; a folder or device is refused.
        let target = d.path().join("target");
        std::fs::write(&target, "keep me").unwrap();
        let link = d.path().join("link.conf");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        s.exec(&format!("save {}", link.display())).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep me");
        assert!(!std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let e = format!("{:#}", s.exec(&format!("save {}", d.path().display())).unwrap_err());
        assert!(e.contains("not a plain file"), "a folder: {e}");
        assert!(format!("{:#}", s.exec("save /dev/null").unwrap_err()).contains("not a plain file"));
        assert!(std::fs::read_dir(d.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")), "no temp file left");
        // Load: not a huge file; not an attachment without content.
        let big = d.path().join("big");
        std::fs::File::create(&big).unwrap().set_len(17 << 20).unwrap();
        assert!(format!("{:#}", s.exec(&format!("load override {}", big.display())).unwrap_err()).contains("not a saved configuration"));
        std::fs::write(&f, "config custom/none.nix;\n").unwrap();
        assert!(format!("{:#}", s.exec(&format!("load override {}", f.display())).unwrap_err()).contains("its content is not in the file"));
    }

    /// From the audit of 2026-10-31: an earlier configuration's software (its lock) is not loaded onto
    /// another source than the one it was locked for.
    #[test]
    fn software_is_rolled_back_only_within_its_source() {
        let (d, m) = (repo_with_software(), Fake::default());
        let flake = |src: &str| format!("printf '{{\\n  inputs = {{\\n    torq.url = \"{src}\";\\n  }};\\n}}\\n' > flake.nix && git add -A && git commit -qm {}", src.replace([':', '/'], "-"));
        sh(d.path(), &flake("github:a/torq"));
        sh(d.path(), &flake("github:b/torq"));
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", s.exec("request system software rollback 1").unwrap_err());
        assert!(e.contains("came from github:a/torq, not github:b/torq"), "{e}");
    }

    /// From the second audit (M3): a folder as the source only in dev mode — at `set`, and at
    /// `commit` if dev mode is turned off with a folder still the source.
    #[test]
    fn a_folder_as_the_source_only_in_dev_mode() {
        let (d, m) = (repo_with_software(), Fake { answer: true, ..Default::default() });
        sh(d.path(), "printf '{\\n  inputs = {\\n    torq.url = \"github:netstacks/torqos\";\\n  };\\n}\\n' > flake.nix && git add -A && git commit -qm flake");
        let mut s = session(d.path(), None, &m);
        s.exec("configure").unwrap();
        // A rule between settings: said at once, refused at `commit` — never by the order of lines.
        let out = s.exec("set system software torq source path:/home/dev/torq").unwrap();
        assert!(out.contains("! TorqOS from a folder (path:)") && out.contains("torq dev enable true") && out.contains("`commit` refuses until then"), "{out}");
        assert!(s.exec("commit").unwrap_err().to_string().contains("torq dev enable true"));
        assert!(s.exec("commit check").unwrap_err().to_string().contains("torq dev enable true"), "a check applies the same rules");
        assert!(m.commits.borrow().is_empty());
        // Nothing to load is never a configuration: an empty file or paste loads nothing.
        let empty = d.path().join("empty.conf");
        std::fs::write(&empty, "\n  \n").unwrap();
        assert!(format!("{:#}", s.exec(&format!("load override {}", empty.display())).unwrap_err()).contains("the file is empty"));
        run(&mut s, &["set system software torq source github:netstacks/torqos/v1"]);
        // In dev mode it may; turning dev mode off with the folder still the source does not commit.
        run(&mut s, &["rollback 0", "set torq dev enable true", "set system software torq source path:/home/dev/torq", "set torq dev enable false"]);
        let e = s.exec("commit").unwrap_err().to_string();
        assert!(e.contains("torq dev enable true"), "{e}");
        assert!(m.commits.borrow().is_empty());
        // A saved configuration holds `system software …` before `torq dev …` (sorted): it loads back
        // whole, its lines in any order — the rule is checked on the result, not line by line.
        let file = d.path().join("dev.conf");
        run(&mut s, &["set torq dev enable true", &format!("save {}", file.display()), "rollback 0"]);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.find("source path:/home/dev/torq").unwrap() < text.find("dev {").unwrap(), "{text}");
        let out = s.exec(&format!("load override {}", file.display())).unwrap();
        assert!(out.ends_with("load complete\n") && !out.contains("refuses"), "{out}");
        assert!(s.exec("show system software").unwrap().contains("path:/home/dev/torq"));
    }

    /// From the audit of 2026-10-31: a deleted Nix file is saved as deleted (not resurrected by a
    /// load); `load override` takes an attached file's content from the file only, never the
    /// machine's own; `save` refuses a file whose name `load` could not read back.
    #[test]
    fn a_saved_configuration_is_the_candidate_exactly() {
        let (d, m) = (repo_with_software(), Fake::default());
        sh(d.path(), "mkdir -p custom && printf '{ ... }: { }\\n' > custom/a.nix && printf '{ ... }: { }\\n' > custom/old.nix && printf '{ \"\": \"custom/a.nix\" }\\n' > config-files.json && git add -A && git commit -qm nix");
        let f = d.path().join("saved.conf");
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "file delete custom/old.nix", &format!("save {}", f.display())]);
        assert!(std::fs::read_to_string(&f).unwrap().contains("## config file deleted custom/old.nix\n"));
        let wanted = s.exec("show | compare").unwrap();
        run(&mut s, &["rollback 0", &format!("load override {}", f.display())]);
        assert_eq!(s.exec("show | compare").unwrap(), wanted);
        assert!(s.exec("file list").unwrap().contains("custom/old.nix (deleted in the candidate)"));
        // A file without the attached one's content (cut short, or hand-made): override refuses it,
        // though this machine has a custom/a.nix; merge may use the machine's.
        std::fs::write(&f, "config custom/a.nix;\n").unwrap();
        let e = format!("{:#}", s.exec(&format!("load override {}", f.display())).unwrap_err());
        assert!(e.contains("its content is not in the file"), "{e}");
        s.exec(&format!("load merge {}", f.display())).unwrap();
        // A Nix file whose name could not be loaded back: save says so, rather than write it.
        sh(d.path(), "printf '{ }\\n' > 'bad name.nix' && git add -A && git commit -qm bad");
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], &format!("save {}", f.display())));
        assert!(e.contains("bad name.nix") && e.contains("cannot hold"), "{e}");
        // Bigger than `load` reads: refused at `save`, not written to fail later.
        let (d, m) = (repo(), Fake { edit_to: Some("#".repeat(crate::saved::MAX + 1)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure", "file edit big.nix"], &format!("save {}", d.path().join("s").display())));
        assert!(e.contains("at most") && !d.path().join("s").exists(), "{e}");
    }

    #[test]
    fn ssh_by_any_path_is_said_as_it_is() {
        // `services openssh`, or NixOS's `sshd` alias for it, with passwords allowed: TorqOS's protections
        // apply either way, and the question says passwords are allowed (keys or not).
        for line in ["set services openssh enable true", "set services sshd enable true"] {
            let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
            let mut s = session(d.path(), None, &m);
            run(&mut s, &["configure", line, "set services openssh settings PasswordAuthentication true"]);
            s.exec("commit").unwrap_or_else(|e| panic!("{line}: {e:#}"));
            let q = m.asked.borrow()[0].clone();
            assert!(q.contains("! SSH will accept passwords as well as keys") && q.contains("repeated wrong guesses are blocked"), "{line}: {q}");
        }
        // What the evaluation says counts, not what the files look like: already on, nothing to say.
        let on = Ssh { on: true, passwords: true };
        let (d, m) = (repo(), Fake { answer: true, ssh_state: Some((on, on)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set services openssh ports [ 2222 ]", "commit"]);
        assert!(!m.asked.borrow()[0].contains("SSH will accept"), "{}", m.asked.borrow()[0]);
        // Any system-tier change is evaluated (a software update too: audit of 2026-10-31); only a
        // person's own data files, which cannot touch SSH, are not.
        let (d, m) = (repo_with_software(), Fake { answer: true, ssh_state: Some((Ssh::default(), on)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "run request system update", "commit"]);
        assert_eq!(*m.ssh_calls.borrow(), 1, "a software update is evaluated for SSH");
        let (d, m) = (repo(), Fake { answer: true, ssh_state: Some((Ssh::default(), on)), ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users users dev packages [ htop ]", "commit"]);
        assert_eq!(*m.ssh_calls.borrow(), 0, "a person's own packages cannot turn SSH on");
    }

    #[test]
    fn fenced_and_reserved_in_the_session() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.exec("configure").unwrap();
        for (line, why) in [
            ("set torq broker rebuildCommand x", "not settable in the configuration tree"),
            ("set environment etc torq/broker.json text x", "not settable in the configuration tree"),
            ("set services httpd virtualHosts _x forceSSL true", "the tree holds data only"),
        ] {
            assert!(format!("{:#}", s.exec(line).unwrap_err()).contains(why), "{line}");
        }
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
", "nothing changed");
    }

    #[test]
    fn a_raw_diff_between_commits_is_every_file_as_committed() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "printf '{\"nodes\":{}}' > flake.lock && printf '{ }\\n' > flake.nix && git add -A && git commit -qm software");
        let mut s = session(d.path(), None, &m);
        // (1 is the base, as on a network device: what 0 adds is `+`.)
        let raw = s.exec("show system rollback 1 compare 0 | display diff").unwrap();
        assert!(raw.contains("+++ b/flake.lock\n") && raw.contains("+{\"nodes\":{}}") && raw.contains("+++ b/flake.nix\n"), "{raw}");
        assert!(!raw.contains("host.json"), "only what changed: {raw}");
        assert_eq!(s.exec("show system rollback 0 compare 1").unwrap(), "No differences: configurations 0 and 1 have the same settings.\n", "the tree did not change");
    }

    #[test]
    fn display_detail_names_each_users_files() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        // By its NixOS path, and by its old name: the same file.
        run(&mut s, &["configure", "set users users dev packages htop", "set users dev packages jq"]);
        let out = s.exec("show users | display detail").unwrap();
        assert!(out.contains("## users/dev/packages.json: users users dev packages\n") && out.contains("packages [ htop jq ];"), "{out}");
        assert!(!out.contains("system/"), "only what is shown: {out}");
        let net = s.exec("show networking | display detail").unwrap();
        assert!(net.contains("## system/host.json: networking hostName\n") && !net.contains("users/"), "{net}");
    }

    #[test]
    fn display_detail_and_diff_show_what_is_behind() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        let head = s.repo.head().unwrap();
        let first = s.repo.history(2).unwrap()[1].id.clone();
        assert_eq!(
            s.exec("show configuration | display detail").unwrap(),
            format!("networking {{\n    hostName two;\n}}\n## commit {head}\n## system/host.json: networking hostName\n"),
            "only the settings set (no time-zone here)"
        );
        assert!(s.exec("show configuration users | display detail").unwrap().ends_with(&format!("## commit {head}\n")), "no files below users");
        let commits = s.exec("show system commit | display detail").unwrap();
        assert!(commits.contains(&format!("\n    commit {head}\n")) && commits.ends_with(&format!("    commit {first}\n")), "{commits}");
        assert!(s.exec("show system rollback 1 | display detail").unwrap().contains(&format!("hostName one;\n}}\n## commit {first} (rollback 1)\n")));
        let raw = s.exec("show system rollback 1 compare 0 | display diff").unwrap();
        assert_eq!(raw, "--- a/system/host.json\n+++ b/system/host.json\n@@ -1 +1 @@\n-{ \"host-name\": \"one\" }\n+{ \"host-name\": \"two\" }\n", "the files as committed");
        assert_eq!(s.exec("show system alarms | display detail").unwrap(), "all well\n  running system /nix/store/x\n");
        assert_eq!(s.exec("show system alarms").unwrap(), "all well\n");
        run(&mut s, &["configure", "set system host-name three"]);
        let raw = s.exec("show | compare | display diff").unwrap();
        assert!(raw.contains("+++ b/system/host.json") && raw.contains("+  \"host-name\": \"three\""), "{raw}");
        assert_eq!(s.exec("show | compare | display diff").unwrap(), {
            // What `show | compare | display diff` shows is exactly what commit proposes.
            s.exec("commit").unwrap();
            m.commits.borrow()[0].0.clone()
        });
        run(&mut s, &["set system host-name four"]);
        let detail = s.exec("show networking | display detail").unwrap();
        assert!(detail.contains("hostName four;") && detail.contains(&format!("## the candidate, from commit {head}\n")), "{detail}");
    }

    #[cfg(target_os = "linux")] // the filters' sandbox
    #[test]
    fn filters_work_on_what_a_show_prints() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show configuration | grep host").unwrap(), "    hostName two;\n");
        assert_eq!(s.exec("show configuration | grep -v host | wc -l").unwrap(), "2\n");
        assert_eq!(s.exec("show configuration | grep nothing").unwrap(), "", "nothing matched: no error");
        assert_eq!(s.exec("show system commit | wc -l").unwrap(), "3\n", "a heading and two configurations");
        assert_eq!(s.exec("show configuration | display set | grep -o 'hostName [a-z]*'").unwrap(), "hostName two\n");
        run(&mut s, &["configure", "set system host-name three", "set system time-zone UTC"]);
        assert_eq!(s.exec("show | compare | grep '^+'").unwrap(), "+   hostName three;\n+   timeZone UTC;\n");
        assert_eq!(s.exec("show time | grep -o 'timeZone [A-Z]*' | tr A-Z a-z").unwrap(), "timezone utc\n");
        assert_eq!(s.exec("run show configuration | grep host").unwrap(), "    hostName two;\n", "run: the active one, filtered");
        assert_eq!(s.exec("run show configuration | grep -c host").unwrap(), "1\n");
        assert!(format!("{:#}", s.exec("show | compare | grep -Q x").unwrap_err()).starts_with("| grep: "));
        assert!(s.exec("set system host-name four | wc -l").is_err());
        assert_eq!(s.exec("show | grep host").unwrap(), "    hostName three;\n", "the refused line changed nothing");
    }

    #[test]
    fn a_user_change_commits_without_asking() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        let out = run(&mut s, &["configure", "set users dev packages jq", "show | compare"]);
        assert!(out.contains("[edit users users dev]\n+   packages [ jq ];\n"), "{out}");
        let out = s.exec("commit").unwrap();
        assert_eq!(out, "commit complete (change #42)\n");
        let c = m.commits.borrow();
        let (diff, base, tier, reason, confirm) = &c[0];
        assert!(diff.contains("+++ b/users/dev/packages.json") && diff.contains("+  \"jq\""), "{diff}");
        assert_eq!((base.as_str(), tier.as_str(), reason.as_str(), *confirm), (s.repo.head().unwrap().as_str(), "user", "+ users users dev packages [ jq ];", None));
        assert!(m.asked.borrow().is_empty(), "user tier: no question");
        assert_eq!(m.checks.borrow().len(), 1, "checked before committing");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
", "the candidate starts over after a commit");
    }

    #[test]
    fn a_system_change_needs_a_person() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("needs a person to approve it"), "{e}");
        assert!(m.commits.borrow().is_empty());
        assert!(m.asked.borrow()[0].contains("-   hostName two;\n+   hostName three;"), "the question shows the change");
        // The candidate is kept for another try.
        assert!(s.exec("show | compare").unwrap().contains("hostName three"));
        let m = Fake { answer: true, ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three", "commit comment \"rename\""]);
        assert_eq!((m.commits.borrow()[0].2.as_str(), m.commits.borrow()[0].3.as_str()), ("system", "rename"));
    }

    #[test]
    fn a_stale_candidate_is_refused_until_rollback_0() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        sh(d.path(), "printf '{ \"host-name\": \"elsewhere\" }\\n' > system/host.json && git commit -qam third");
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("has changed since your candidate") && e.contains("rollback 0"), "{e}");
        assert!(format!("{:#}", s.exec("commit check").unwrap_err()).contains("rollback 0"));
        assert!(m.commits.borrow().is_empty());
        run(&mut s, &["rollback 0", "set system host-name three", "commit"]);
        assert_eq!(m.commits.borrow().len(), 1);
    }

    #[test]
    fn rollback_loads_an_earlier_configuration() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "rollback 1"]), "Entering configuration mode\nLoaded configuration 1: 1 change from the configuration in effect (`show | compare` lists them; `commit` makes it the machine's).\n");
        assert_eq!(s.exec("show | compare").unwrap(), "[edit networking]\n-   hostName two;\n+   hostName one;\n");
        assert!(format!("{:#}", s.exec("rollback 9").unwrap_err()).contains("there is no configuration 9: there are 2 (0 to 1;"));
        assert!(format!("{:#}", s.exec("rollback x").unwrap_err()).contains("not a number"));
        for huge in ["rollback 18446744073709551615", "run show system rollback 18446744073709551615"] {
            assert!(format!("{:#}", s.exec(huge).unwrap_err()).contains("configuration numbers go from 0 to 10000"), "{huge}");
        }
    }

    #[test]
    fn commit_check_and_commit_confirmed() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system time-zone UTC"]);
        assert_eq!(s.exec("commit check").unwrap(), "configuration check succeeds\n");
        assert_eq!(m.checks.borrow()[0], [("system/host.json".to_string(), Some("{\n  \"host-name\": \"two\",\n  \"time-zone\": \"UTC\"\n}\n".to_string()))]);
        assert!(m.commits.borrow().is_empty(), "check commits nothing");
        let out = s.exec("commit confirmed 5").unwrap();
        assert!(out.contains("rolled back in 5 minutes"), "{out}");
        assert_eq!(m.commits.borrow()[0].4, Some(5));
        // Committing again, with nothing changed, confirms it.
        let m = Fake { pending: waiting("dev"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "commit"]), "Entering configuration mode\ncommit complete: change #42 confirmed (#43)\n");
        assert_eq!(*m.confirmed.borrow(), 1);
        let m = Fake::default();
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "commit"]), "Entering configuration mode\ncommit complete (no changes)\n");
    }

    #[test]
    fn a_saved_candidate_outlives_the_session_a_private_one_does_not() {
        let (d, m) = (repo(), Fake::default());
        let state = tempfile::tempdir().unwrap();
        let saved = state.path().join("candidate.json");
        let mut s = session(d.path(), Some(saved.clone()), &m);
        let out = run(&mut s, &["configure", "set users dev packages htop", "exit"]);
        assert!(out.contains("The candidate has changes not yet committed: they are kept"), "{out}");
        assert_eq!(s.mode, Mode::Operational);
        let mut s = session(d.path(), Some(saved.clone()), &m);
        assert!(run(&mut s, &["configure", "show | compare"]).contains("+   packages [ htop ];"));
        // `torqcli -c`: a private candidate; nothing is saved.
        let mut p = session(d.path(), None, &m);
        run(&mut p, &["configure", "set users dev packages jq"]);
        assert!(!fs_has(state.path(), "\"jq\""));
    }

    /// While a commit confirmed waits, a commit with changes is refused before anything is evaluated,
    /// asked or proposed; only its author confirms it (review of 4a12b45).
    #[test]
    fn a_waiting_commit_confirmed_comes_first() {
        let (d, m) = (repo(), Fake { answer: true, pending: waiting("dev"), ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("change #42 (commit confirmed) is waiting"), "{e}");
        assert!(m.checks.borrow().is_empty() && m.asked.borrow().is_empty() && m.commits.borrow().is_empty());
        let m = Fake { pending: waiting("ana"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "commit"));
        assert!(e.contains("waiting for ana to confirm it"), "{e}");
        assert_eq!(*m.confirmed.borrow(), 0);
        let m = Fake { pending: waiting("dev"), ..Fake::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "commit confirmed 5"));
        assert!(e.contains("already waiting for confirmation") && e.contains("`commit` confirms it"), "{e}");
    }

    fn run_err(s: &mut Session, before: &[&str], line: &str) -> anyhow::Error {
        run(s, before);
        s.exec(line).unwrap_err()
    }

    #[test]
    fn commit_options_parse_and_are_bounded() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users dev packages jq"]);
        for (bad, why) in [("commit confirmed 0", "1 to 1440"), ("commit confirmed 1441", "1 to 1440"), ("commit confirmed 99999999999", "1 to 1440"), ("commit now", "one of")] {
            assert!(format!("{:#}", s.exec(bad).unwrap_err()).contains(why), "{bad}");
        }
        assert!(m.checks.borrow().is_empty(), "refused before anything is checked");
        s.exec("commit confirmed 5 comment \"ssh, keys only\"").unwrap();
        let c = m.commits.borrow();
        assert_eq!((c[0].3.as_str(), c[0].4), ("ssh, keys only", Some(5)));
    }

    /// The ledger reason is built from the comparison and starts with `-` for any changed value: it is
    /// passed through as it is (shell.rs gives it to the broker as `--reason=…`).
    #[test]
    fn a_reason_may_start_with_a_dash() {
        let (d, m) = (repo(), Fake { answer: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set system host-name three", "commit"]);
        assert_eq!(m.commits.borrow()[0].3, "- networking hostName two; + networking hostName three;");
    }

    /// Your own folder is yours; anyone else's takes a person's yes, like a system change.
    #[test]
    fn another_users_folder_needs_a_person() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        s.schema.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("ana".into(), String::new());
        run(&mut s, &["configure", "set users ana packages vim"]);
        let e = format!("{:#}", s.exec("commit").unwrap_err());
        assert!(e.contains("needs a person"), "{e}");
        assert!(m.asked.borrow()[0].contains("This changes the settings of: ana."), "{:?}", m.asked.borrow());
        assert!(m.commits.borrow().is_empty());
    }

    /// A refused commit keeps the candidate; `-c`'s exit does not promise to keep it.
    #[test]
    fn a_failed_commit_keeps_the_candidate() {
        let (d, m) = (repo(), Fake { refuse: true, ..Fake::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users dev packages jq"]);
        assert!(format!("{:#}", s.exec("commit").unwrap_err()).contains("busy"));
        assert!(s.exec("show | compare").unwrap().contains("+   packages [ jq ];"));
        assert_eq!(s.exec("exit").unwrap(), "", "a script's candidate: said once, when it ends");
    }

    fn fs_has(dir: &Path, needle: &str) -> bool {
        std::fs::read_dir(dir).unwrap().flatten().any(|e| std::fs::read_to_string(e.path()).unwrap_or_default().contains(needle))
    }

    /// `edit` / `up` / `top` move through the tree; commands are relative to where you are.
    #[test]
    fn moving_through_the_tree() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit services httpd virtualHosts example.com", "set forceSSL true"]);
        assert_eq!(s.edit_path(), w("services httpd virtualHosts example.com"));
        assert_eq!(s.exec("show").unwrap(), "forceSSL true;\n");
        run(&mut s, &["up"]);
        assert_eq!(s.edit_path(), w("services httpd virtualHosts"), "one level: the set of entries is a level of its own");
        run(&mut s, &["up"]);
        assert_eq!(s.edit_path(), w("services httpd"));
        assert!(format!("{:#}", s.exec("edit enable").unwrap_err()).contains("a setting, not a place to go: `set enable <value>`"));
        run(&mut s, &["top", "edit system", "set time-zone UTC"]);
        // (Its NixOS place is `time`, not `system`: seen from the top.)
        assert!(s.exec("top show | compare").unwrap().contains("[edit time]\n+   timeZone UTC;"), "an old name, from where you are");
        run(&mut s, &["top"]);
        assert!(format!("{:#}", s.exec("up").unwrap_err()).contains("already at the top"));
        // `exit` goes up a level, and out of configuration mode at the top.
        run(&mut s, &["edit virtualisation", "exit"]);
        assert_eq!((s.mode, s.edit_path().is_empty()), (Mode::Configuration, true));
        run(&mut s, &["edit virtualisation", "exit configuration-mode"]);
        assert_eq!(s.mode, Mode::Operational);
    }

    /// Deleting a whole container or list entry takes `all`, so a slip cannot take everything below.
    #[test]
    fn delete_takes_everything_below_as_on_a_network_device() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set users users dev packages [ jq ]", "set environment systemPackages [ htop vim ]"]);
        run(&mut s, &["set virtualisation docker enable true", "set virtualisation docker rootless enable true"]);
        // `all` is no word of `delete` any more: here it would be a setting's name.
        assert!(s.exec("delete virtualisation docker enable all").is_err());
        // From where you are: a place goes whole; a value alone; a whole list.
        run(&mut s, &["edit virtualisation docker"]);
        assert_eq!(s.exec("delete rootless").unwrap(), "");
        run(&mut s, &["top", "delete environment systemPackages htop"]);
        assert!(s.exec("show | compare").unwrap().contains("vim") && !s.exec("show | compare").unwrap().contains("htop"));
        run(&mut s, &["delete environment systemPackages", "delete users users dev packages"]);
        let c = s.exec("show | compare").unwrap();
        assert_eq!(c, "[edit virtualisation docker]\n+   enable true;\n");
        // What is not set: a warning, nothing deleted, no error (a paste goes on).
        assert_eq!(s.exec("delete time timeZone").unwrap(), "! nothing deleted: time timeZone is not set\n");
        assert_eq!(s.exec("delete virtualisation docker rootless").unwrap(), "! nothing deleted: virtualisation docker rootless is not set\n");
        assert_eq!(s.exec("show | compare").unwrap(), c, "nothing changed");
        assert!(s.exec("delete nonsense").unwrap_err().to_string().contains("no such setting"), "an unknown path is still an error");
    }

    /// `delete` with nothing after it is everything here: asked first; refused without a yes.
    /// `up <n>`, `top <command>` (back where you were after it, unless it moves), `commit and-quit`, and
    /// no `run configure` inside configuration mode.
    /// `show | compare` from where configuration mode is: what changes there and below it.
    #[test]
    fn compare_at_the_edit_level() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "set time timeZone UTC", "set services httpd virtualHosts \"a b\" documentRoot /srv", "edit networking", "set hostName three"]);
        assert_eq!(s.exec("show | compare").unwrap(), "[edit networking]\n-   hostName two;\n+   hostName three;\n");
        run(&mut s, &["top", "edit services httpd virtualHosts \"a b\""]);
        assert!(s.exec("show | compare").unwrap().contains("documentRoot /srv"), "a quoted name in a section");
        run(&mut s, &["top"]);
        let all = s.exec("show | compare").unwrap();
        assert!(all.contains("hostName three") && all.contains("timeZone UTC") && all.contains("documentRoot"), "{all}");
        assert!(s.reviews("show | compare") && !{ s.exec("edit networking").unwrap(); s.reviews("show | compare") }, "only the whole is a review");
    }

    /// Whether a line commits, or reviews, is read as the line runs: abbreviated, or from the top —
    /// no form slips past the question after failed lines (review of 91bbf66).
    #[test]
    fn commits_and_reviews_whatever_their_form() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit networking"]);
        for line in ["commit", "com", "comm and", "commit a", "top commit", "to com", "commit comment x"] {
            assert!(s.commits(line), "{line}");
        }
        for line in ["commit check", "comm ch", "show | compare", "commi" , "top commit check"] {
            assert_eq!(s.commits(line), line == "commi", "{line}");
        }
        assert!(!s.reviews("show | compare"), "a part is no review");
        assert!(s.reviews("top show | compare") && s.reviews("to sh | comp"), "from the top: the whole");
        // An empty word is never another word (`exit ""` is not `exit configuration-mode`).
        assert!(s.exec_words(&["exit".to_string(), String::new()]).is_err());
        assert_eq!(s.edit_path(), w("networking"));
        // `top <command>` comes back; `top commit and-quit` leaves configuration mode for good.
        run(&mut s, &["top show time"]);
        assert_eq!(s.edit_path(), w("networking"), "back where it was");
        // At a level where nothing changes but elsewhere something does: said.
        run(&mut s, &["top set time timeZone UTC"]);
        assert!(s.exec("show | compare").unwrap().contains("nothing changes here; elsewhere it does"));
    }

    /// `request system password [<user>]`: your own, or another person's (the broker sets it).
    #[test]
    fn a_password_is_requested_for_you_or_someone() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("request system password").unwrap(), "password of dev\n");
        assert_eq!(s.exec("req sys pass alice").unwrap(), "password of alice\n");
    }

    #[test]
    fn moving_as_on_a_network_device() {
        let (d, m) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit services httpd virtualHosts example.com", "up 9"]);
        assert!(s.edit_path().is_empty(), "up as far as there is: {:?}", s.edit_path());
        run(&mut s, &["edit networking", "top set time timeZone UTC"]);
        assert_eq!(s.edit_path(), w("networking"), "back where it was");
        assert!(s.exec("top show time").unwrap().contains("timeZone UTC"));
        run(&mut s, &["top edit virtualisation"]);
        assert_eq!(s.edit_path(), w("virtualisation"), "a move moves");
        assert!(format!("{:#}", s.exec("run configure").unwrap_err()).contains("'configure' is not a word here (after `run`)"));
        assert!(format!("{:#}", s.exec("edit").unwrap_err()).contains("needs <path> (where to go)"));
        assert!(format!("{:#}", s.exec("set").unwrap_err()).contains("needs <path> (the setting and its value)"));
        assert!(format!("{:#}", s.exec("edit nosuch").unwrap_err()).contains("nosuch"));
        let out = s.exec("commit and-quit").unwrap();
        assert!(out.ends_with("Exiting configuration mode\n") && s.mode == Mode::Operational, "{out}");
        assert_eq!(m.commits.borrow().len(), 1);
    }

    #[test]
    fn a_bare_delete_asks_first() {
        let (d, no) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &no);
        run(&mut s, &["configure", "set virtualisation docker enable true", "edit virtualisation"]);
        let e = format!("{:#}", s.exec("delete").unwrap_err());
        assert_eq!(e, "not deleted: this needs a person to approve it at a terminal", "a script cannot answer: refused");
        assert_eq!(no.asked.borrow().as_slice(), ["Delete everything under virtualisation? "]);
        assert!(s.exec("show | compare").unwrap().contains("enable true"), "nothing deleted");
        // With a yes, everything under it goes.
        let (d, yes) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &yes);
        run(&mut s, &["configure", "set virtualisation docker enable true", "edit virtualisation", "delete", "top"]);
        assert_eq!(yes.asked.borrow().as_slice(), ["Delete everything under virtualisation? "]);
        assert!(!s.exec("show").unwrap().contains("docker"), "{}", s.exec("show").unwrap());
        // At the top it is the whole configuration, asked as such.
        let (d, no) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &no);
        assert!(run_err(&mut s, &["configure"], "delete").to_string().starts_with("not deleted"), "refused: a script cannot answer");
        assert_eq!(no.asked.borrow().as_slice(), ["Delete the whole configuration? "]);
        // With a yes there, everything goes: TorqOS's own files and nixos/ alike.
        let (d, yes) = (repo(), Fake { answer: true, ..Default::default() });
        let mut s = session(d.path(), None, &yes);
        run(&mut s, &["configure", "set virtualisation docker enable true", "set system time-zone UTC", "delete"]);
        assert_eq!(s.exec("show").unwrap(), "", "nothing left");
    }

    /// `load … terminal`: what is pasted, up to Ctrl-D — brace format for override and merge, `set`
    /// lines for `load set`; all or nothing as from a file.
    #[test]
    fn load_from_the_terminal() {
        let d = repo();
        let m = Fake { typed: Some("/* pasted */\nvirtualisation {\n    docker {\n        enable true;\n    }\n}\n".into()), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        assert_eq!(run(&mut s, &["configure", "load merge terminal"]).lines().last(), Some("load complete"));
        let c = s.exec("show | compare").unwrap();
        assert!(c.contains("[edit virtualisation docker]\n+   enable true;"), "{c}");
        let m = Fake { typed: Some("set environment variables A one\n# a note\nset environment variables B two\n".into()), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        assert!(run(&mut s, &["configure", "load set terminal"]).contains("load complete (2 lines)"));
        assert!(s.exec("show | compare").unwrap().contains("+   B two;"));
        let m = Fake { typed: Some("set environment variables A one\nset nonsense x\n".into()), ..Default::default() };
        let mut s = session(d.path(), None, &m);
        let e = format!("{:#}", run_err(&mut s, &["configure"], "load set terminal"));
        assert!(e.starts_with("nothing loaded: line 2: set nonsense x"), "the line is named once, by the shell: {e}");
        assert_eq!(s.exec("show | compare").unwrap(), "No changes: the candidate is the configuration in effect.
", "all or nothing");
        // No terminal (a script): said.
        let m = Fake::default();
        let mut s = session(d.path(), None, &m);
        assert!(format!("{:#}", run_err(&mut s, &["configure"], "load merge terminal")).contains("no terminal"));
    }

    /// Out of an entry is into its set, a place of its own (as a network device's `interfaces`); a
    /// command word cannot name an entry (review of f705cc8).
    #[test]
    fn up_goes_one_level_at_a_time() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        run(&mut s, &["configure", "edit services httpd virtualHosts example.com", "up"]);
        assert_eq!(s.edit_path(), w("services httpd virtualHosts"));
        assert!(s.exec("set example.org forceSSL true").is_ok(), "an entry, from its set");
        run(&mut s, &["up", "up", "up"]);
        assert!(s.edit_path().is_empty());
        // `config` as an entry's name would be ambiguous (`<place> config <file>`); `all` is a name now.
        assert!(format!("{:#}", s.exec("set services httpd virtualHosts config forceSSL true").unwrap_err()).contains("keeps the word for a command"));
        s.schema.nodes.get_mut("users").unwrap().values.as_mut().unwrap().insert("config".into(), String::new());
        assert!(format!("{:#}", s.exec("set users config packages vim").unwrap_err()).contains("keeps the word for a command"), "the old names too");
        run(&mut s, &["set services httpd virtualHosts all forceSSL true"]);
    }

    #[test]
    fn earlier_configurations_and_their_differences() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show system rollback 1").unwrap(), "networking {\n    hostName one;\n}\n");
        // As on a network device: 0 is the base, `+` is what 1 has.
        assert_eq!(s.exec("show system rollback 0 compare 1").unwrap(), "[edit networking]\n-   hostName two;\n+   hostName one;\n");
        assert_eq!(s.exec("show system rollback 1 compare 0").unwrap(), "[edit networking]\n-   hostName one;\n+   hostName two;\n");
        assert!(format!("{:#}", s.exec("show system rollback 5").unwrap_err()).contains("there is no configuration 5: there are 2 (0 to 1;"));
        assert!(format!("{:#}", s.exec("show system rollback x").unwrap_err()).contains("not a number"));
    }

    #[test]
    fn services_alarms_packages_and_requests() {
        let (d, m) = (repo(), Fake::default());
        sh(d.path(), "echo '[\"ssh\"]' > services.json && git add -A && git commit -qm ssh");
        let mut s = session(d.path(), None, &m);
        let out = s.exec("show services").unwrap();
        assert!(out.contains("  services openssh") && out.contains("To move them: `set services openssh enable true`, `delete torq services enabled`, then commit."), "{out}");
        assert_eq!(s.exec("show system alarms").unwrap(), "all well\n");
        assert_eq!(s.exec("show packages ripgrep").unwrap(), "searched ripgrep\n");
        assert_eq!(s.exec("request system repair").unwrap(), "repaired\n");
        let e = format!("{:#}", s.exec("request system software rollback 0").unwrap_err());
        assert!(e.contains("configuration 0 has no flake.lock"), "{e}");
        let out = s.exec("request system update").unwrap();
        assert!(out.contains("+   torq ") && !out.contains("-   torq") && !out.contains("downgrade"), "a repo without a flake.lock: torq comes, nothing goes, no downgrade claimed: {out}");
        assert_eq!(s.exec("config").unwrap().lines().next(), Some("Entering configuration mode"));
    }

    #[test]
    fn operational_mode_shows_the_active_configuration() {
        let (d, m) = (repo(), Fake::default());
        let mut s = session(d.path(), None, &m);
        assert_eq!(s.exec("show configuration").unwrap(), "networking {\n    hostName two;\n}\n");
        assert_eq!(s.exec("show configuration | display set").unwrap(), "set networking hostName two\n");
        assert!(s.exec("show system commit").unwrap().lines().nth(1).is_some_and(|l| l.starts_with("0 ")));
        assert!(format!("{:#}", s.exec("set system host-name x").unwrap_err()).contains("'set' is a configuration-mode command: `configure` first"));
        run(&mut s, &["configure", "set system host-name three"]);
        assert_eq!(s.exec("run show configuration").unwrap(), "networking {\n    hostName two;\n}\n", "run: the active one");
        assert_eq!(s.mode, Mode::Configuration);
        s.exec("exit").unwrap();
        s.exec("exit").unwrap();
        assert!(s.done);
    }
}
