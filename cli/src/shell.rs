//! `torqcli` (ADR-016): the shell itself — the real `Machine` behind a session (Nix evaluation and
//! the broker) and the entry point. The commands are in
//! session.rs, the tree in tree.rs, the candidate in candidate.rs.

use crate::candidate::{Candidate, Repo};
use crate::session::{Configuration, Evaluated, History, Machine, Mode, Session, SshFacts, Waiting};
use crate::tree::Schema;
use crate::{broker, check_repo, evaluate, history, me, run, scratch_dir, Cli, BROKER, REPO};
use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// The schema of the running system (modules/schema.nix).
pub(crate) const SCHEMA: &str = "/etc/torq/schema.json";
/// Every NixOS setting of the running system (modules/nixos-schema.nix): the tree's paths.
pub(crate) const NIXOS_SCHEMA: &str = "/etc/torq/nixos-schema.json";

pub(crate) struct Real<'a> {
    cli: &'a Cli,
    /// `| display detail`: steps and Nix's and the broker's output shown as they come (no spinner).
    detail: std::cell::Cell<bool>,
    /// Who answers what a command asks a person: in TorqOS Settings, its dialog (None: the terminal).
    asker: Option<Box<dyn Fn(&str) -> crate::session::Answer + 'a>>,
}

impl Real<'_> {
    /// The committed configuration at `base`, as the broker will build it (not the repo's working
    /// copy), with `files` changed (None: deleted), in a scratch folder (kept while the first is).
    fn changed_copy(&self, base: &str, files: &[(String, Option<String>)]) -> Result<(tempfile::TempDir, PathBuf)> {
        let scratch = scratch_dir()?;
        let copy = scratch.path().join("config");
        fs::create_dir(&copy)?;
        Repo::new(&self.cli.repo).archive(base, &copy)?;
        for (path, content) in files {
            no_link_on_the_way(&copy, path)?;
            let f = copy.join(path);
            match content {
                Some(c) => {
                    fs::create_dir_all(f.parent().context("a tree file has a folder")?)?;
                    fs::write(&f, c)?;
                }
                None => {
                    let _ = fs::remove_file(&f);
                }
            }
        }
        Ok((scratch, copy))
    }

    /// If reverting `target` turns SSH on, what the person approving must see — as at a commit: the
    /// keys that get in (none refuses, for the catalog's), or that it is outside the catalog. Read from
    /// the whole configuration as the revert leaves it, evaluated (every way SSH can be turned on, a
    /// config file too; audit of 2026-10-31): the change's reverse patch applied to HEAD.
    fn revert_ssh_keys(&self, target: &history::Entry) -> Result<String> {
        let Some(after) = target.data["commit"].as_str() else { return Ok(String::new()) };
        let repo = Repo::new(&self.cli.repo);
        let head = repo.head()?;
        let Some(before) = repo.parent_of(after)? else { return Ok(String::new()) };
        let touched: Vec<&str> = target.data["files"].as_array().into_iter().flatten().filter_map(|v| v.as_str()).collect();
        // A person's own data files cannot turn SSH on.
        if touched.iter().all(|p| crate::tree::is_user_data(p)) {
            return Ok(String::new());
        }
        // The files as the revert leaves them: its reverse patch on HEAD (later edits elsewhere in a
        // file kept, as git revert keeps them; review of the audit fixes). If it does not apply, the
        // broker refuses the revert as a conflict, and there is nothing to show.
        let Some(files) = repo.reverted_files(&head, &before, after, &touched)? else { return Ok(String::new()) };
        // A folder as the source only in dev mode — a revert must not bring one back after dev mode
        // was turned off (as `commit` checks it).
        let after_revert = |p: &str| -> Result<Option<String>> {
            match files.iter().find(|(f, _)| f == p) {
                Some((_, c)) => Ok(c.clone()),
                None => Ok(repo.file_at(&head, p)?.map(|b| String::from_utf8_lossy(&b).into_owned())),
            }
        };
        let source = after_revert("flake.nix")?.and_then(|f| crate::software::source_of(&f));
        let dev = after_revert("nixos/torq.json")?
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.pointer("/dev/enable").and_then(serde_json::Value::as_bool))
            == Some(true);
        if source.as_deref().is_some_and(crate::software::writable_folder) && !dev {
            bail!("not reverted: it would make TorqOS come from a folder (path:) without `torq dev enable true`; root builds from it");
        }
        // Not evaluable: then the broker's build of it fails just the same, and nothing is switched on —
        // said, not a reason to refuse here.
        let after = match self.evaluate(&head, &files) {
            Ok(e) => e.ssh,
            Err(e) => return Ok(format!("(SSH not checked: the configuration it leaves does not evaluate: {})\n", history::one_line(&format!("{e:#}"), 120))),
        };
        // Asked at the terminal: its sentences fit it (ui.rs). As a commit is asked (session.rs), SSH
        // before it kept by commit.
        let ui = crate::ui::Ui::plain_at(crate::ui::fit(crate::ui::columns(false)));
        let cache = crate::evalcache::Cache::personal();
        crate::session::ssh_review(self, || crate::session::ssh_before(self, &cache, &head), &after, "reverted", &ui)
    }

    /// The progress line, unless detail shows each step instead.
    fn progress(&self, phase: &str) -> Option<run::Progress> {
        (!self.detail.get()).then(|| run::Progress::start(phase))
    }


    /// The broker's status as `show system alarms` reports it (`crate::status_report`).
    fn status_report(&self, fit: Option<usize>) -> Result<(serde_json::Value, crate::status::Report)> {
        crate::status_report(self.cli, fit, run::start_log()?)
    }
}

/// A broker outcome as a result, its message cleaned for the terminal.
fn done(o: run::Outcome, what: &str) -> Result<serde_json::Value> {
    match o {
        run::Outcome::Done(v) => Ok(v),
        run::Outcome::Busy => bail!("{what}: another change is being made right now; try again in a moment"),
        run::Outcome::Refused(m) => bail!("{what} refused: {}", history::one_line(&m, 300)),
        run::Outcome::Broken(m) => bail!("{what}: {m}"),
    }
}

impl<'a> Real<'a> {
    /// The machine, as TorqOS Settings' sessions use it too (app.rs).
    pub(crate) fn new(cli: &'a Cli) -> Self {
        Real { cli, detail: std::cell::Cell::new(false), asker: None }
    }

    /// The machine, its questions answered by `asker`.
    pub(crate) fn asking(cli: &'a Cli, asker: Box<dyn Fn(&str) -> crate::session::Answer + 'a>) -> Self {
        Real { cli, detail: std::cell::Cell::new(false), asker: Some(asker) }
    }
}

impl Machine for Real<'_> {
    fn needs(&self) -> Option<String> {
        let session = crate::app::session_recorded();
        let needs = crate::app::needs(std::path::Path::new("/run/booted-system"), std::path::Path::new("/run/current-system"), session.as_deref());
        needs.line().or_else(|| needs.note())
    }

    fn evaluate(&self, base: &str, files: &[(String, Option<String>)]) -> Result<Evaluated> {
        let (_scratch, copy) = self.changed_copy(base, files)?;
        let log = run::start_log()?;
        let _p = self.progress("Checking");
        evaluate(&copy, &log, self.detail.get())
    }

    fn ssh_at(&self, base: &str) -> Result<SshFacts> {
        let (_scratch, copy) = self.changed_copy(base, &[])?;
        let log = run::go_on_log()?;
        let _p = self.progress("Reading the SSH settings");
        crate::ssh_state(&copy, &log, self.detail.get())
    }

    fn set_detail(&self, on: bool) {
        self.detail.set(on);
    }

    fn detail(&self) -> bool {
        self.detail.get()
    }

    fn show(&self, line: &str) {
        println!("{}", history::clean(line));
        let _ = std::io::stdout().flush();
    }

    fn remote(&self) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["remote"])?, &log)?, "show system remote")?;
        let Some(url) = v["url"].as_str() else {
            return Ok("No remote: this machine's configuration and its ledger are kept here only (torq remote url <ssh url> names one).\n".into());
        };
        let mut out = format!("Remote: {}\n", history::one_line(url, 120));
        if let Some(why) = v["alarm"]["why"].as_str() {
            out.push_str(&format!(
                "  ⚠ This machine's ledger and the remote's disagree (since {}): {}\n    Nothing more is pushed until a person looks into it; then: request system remote clear.\n",
                history::one_line(v["alarm"]["at"].as_str().unwrap_or("?"), 30),
                history::one_line(why, 300)
            ));
        }
        if !v["key"].as_bool().unwrap_or(false) {
            out.push_str("  No key yet: request system remote key makes one, to add to the repository as a deploy key.\n");
        }
        let last = &v["last"];
        match last["ok"].as_bool() {
            None => out.push_str("  Nothing pushed yet.\n"),
            Some(ok) => {
                let at = history::one_line(last["at"].as_str().unwrap_or("?"), 30);
                if ok {
                    out.push_str(&format!("  Last push: {at}, the configuration and {} ledger entries.\n", last["entries"].as_u64().unwrap_or(0)));
                } else {
                    out.push_str(&format!("  Last push: {at}, failed: {}\n", history::one_line(last["error"].as_str().unwrap_or("?"), 200)));
                }
            }
        }
        if let Some(why) = last["main"].as_str() {
            out.push_str(&format!("  The configuration ({}) was not pushed: {}\n", "main", history::one_line(why, 200)));
        }
        match last["anchor"]["state"].as_str() {
            Some("agrees") => out.push_str(&format!(
                "  Its ledger: the first {} of this machine's {} entries, as they are here.\n",
                last["anchor"]["remote"].as_u64().unwrap_or(0),
                last["anchor"]["local"].as_u64().unwrap_or(0)
            )),
            Some("differs") => out.push_str(&format!(
                "  At the last push it disagreed: {}\n",
                history::one_line(last["anchor"]["why"].as_str().unwrap_or("?"), 200)
            )),
            Some("unreachable") => out.push_str("  It could not be reached at the last push.\n"),
            _ => {}
        }
        Ok(out)
    }

    fn fetch_incoming(&self) -> Result<Option<String>> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["fetch-incoming"])?, &log)?, "request system pull")?;
        let id = v["incoming"].as_str().map(String::from);
        if id.as_ref().is_some_and(|c| c.len() != 40 || !c.bytes().all(|b| b.is_ascii_hexdigit())) {
            bail!("the broker's answer is not a commit");
        }
        Ok(id)
    }

    fn remote_clear(&self, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["remote-clear", "--approved-by", me])?, &log)?, "request system remote clear")?;
        Ok(format!(
            "The remote's alarm is cleared (ledger #{}): the next push checks its whole history again, and raises it again if it is still true.\n",
            v["seq"].as_u64().unwrap_or(0)
        ))
    }

    fn acknowledge(&self, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["ledger-acknowledge", "--approved-by", me])?, &log)?, "request system ledger acknowledge")?;
        Ok(format!("On the record (ledger #{}): what was said is said no more. A new disagreement is said as ever.\n", v["seq"].as_u64().unwrap_or(0)))
    }

    fn snapshot_take(&self, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["snapshot-take", "--approved-by", me])?, &log)?, "request system snapshot take")?;
        let seq = v["seq"].as_u64().unwrap_or(0);
        Ok(format!("A snapshot of the data, as #{seq}: request system snapshot restore {seq} <path> brings a path back.\n"))
    }

    fn snapshot_delete(&self, change: u64, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let seq = change.to_string();
        let v = done(run::broker_json(broker(self.cli, &["snapshot-delete", &seq, "--approved-by", me])?, &log)?, "request system snapshot delete")?;
        Ok(format!("The snapshot from before #{change} is deleted (ledger #{}).\n", v["seq"].as_u64().unwrap_or(0)))
    }

    fn snapshots(&self) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["snapshots"])?, &log)?, "show system snapshots")?;
        let list = v.as_array().map(Vec::as_slice).unwrap_or_default();
        if list.is_empty() {
            return Ok("No snapshots of the data here (a disk laid out for them has one before each change).\n".into());
        }
        let mut out = String::from("The data as it was before each change (request system snapshot restore <n> <path>):\n");
        for s in list {
            out.push_str(&format!(
                "  #{:<4} {}  {}\n",
                s["change"].as_u64().unwrap_or(0),
                history::one_line(s["ts"].as_str().unwrap_or("?"), 25),
                history::one_line(s["reason"].as_str().unwrap_or(""), 70)
            ));
        }
        Ok(out)
    }

    fn restore_data(&self, change: u64, path: &str, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let seq = change.to_string();
        let v = done(run::broker_json(broker(self.cli, &["restore-data", &seq, "--path", path, "--approved-by", me])?, &log)?, "request system snapshot restore")?;
        let kept = v["data"]["set_aside"].as_str().map_or(String::new(), |a| format!(" What was there is at {}.", history::one_line(a, 120)));
        Ok(format!(
            "{} is back as it was before #{change} (ledger #{}).{kept} A service using it should be restarted.\n",
            history::one_line(path, 120),
            v["seq"].as_u64().unwrap_or(0)
        ))
    }

    fn backups(&self) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["backups"])?, &log)?, "show system backups")?;
        let list = v.as_array().map(Vec::as_slice).unwrap_or_default();
        if list.is_empty() {
            return Ok("No backups are declared (set services restic backups <name> repository <where>: ADR-038).\n".into());
        }
        let text = |b: &serde_json::Value, k: &str, n: usize| history::one_line(b[k].as_str().unwrap_or(""), n);
        let mut out = String::from("Backups (request system restore <backup> [<snapshot>] <path>):\n");
        for b in list {
            let name = text(b, "name", 64);
            out.push_str(&format!("  {name}\n"));
            let run = &b["last_run"];
            if run["running"].as_bool() == Some(true) {
                out.push_str("    running now\n");
            } else if run.is_object() {
                let failed = run["result"].as_str() != Some("success");
                out.push_str(&format!("    last run {}: {}\n", text(run, "at", 40), if failed { format!("failed ({}, status {})", text(run, "result", 30), text(run, "status", 10)) } else { "done".into() }));
                if failed {
                    // What it said, as the journal has it: read as the person asking, as they may.
                    let unit = text(run, "unit", 100);
                    let said = std::process::Command::new("journalctl").args(["-u", &unit, "-n", "3", "-o", "cat", "-q", "--no-pager"]).stderr(std::process::Stdio::null()).output().ok().filter(|o| o.status.success() && !o.stdout.is_empty());
                    match said {
                        Some(o) => String::from_utf8_lossy(&o.stdout).lines().for_each(|l| out.push_str(&format!("      {}\n", history::one_line(l, 160)))),
                        None => out.push_str(&format!("      (journalctl -u {unit} says why)\n")),
                    }
                }
            } else {
                // systemd forgets a unit nothing refers to (a backup with no timer, run by hand): the newest
                // snapshot, restic's own record, says when it last succeeded.
                match b["snapshots"].as_array().and_then(|s| s.iter().filter_map(|x| x["time"].as_str()).max()) {
                    Some(t) => out.push_str(&format!("    last snapshot {} (systemd keeps no record of the run itself)\n", history::one_line(t, 40))),
                    None => out.push_str("    not run since the machine started\n"),
                }
            }
            if let Some(e) = b["error"].as_str() {
                out.push_str(&format!("    {}\n", history::one_line(e, 200)));
            }
            let snaps = b["snapshots"].as_array().map(Vec::as_slice).unwrap_or_default();
            if b["error"].is_null() && snaps.is_empty() {
                out.push_str("    no snapshots yet\n");
            }
            const SHOWN: usize = 10;
            for s in snaps.iter().rev().take(SHOWN) {
                let paths: Vec<String> = s["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(|p| history::one_line(p, 60)).collect();
                out.push_str(&format!("    {}  {}  {}  {}\n", text(s, "id", 8), text(s, "time", 25), text(s, "host", 30), paths.join(" ")));
            }
            if snaps.len() > SHOWN {
                out.push_str(&format!("    … and {} older\n", snaps.len() - SHOWN));
            }
        }
        Ok(out)
    }

    fn secret_value(&self, name: &str, user: Option<&str>, act: &crate::secrets::Act, me: &str) -> Result<String> {
        use crate::secrets::Act;
        let whose = user.map_or_else(|| format!("the system's secret {name}"), |u| format!("{u}'s secret {name}"));
        let mut args: Vec<String> = vec![format!("secret-{}", act.verb()), "--name".into(), name.into()];
        if let Some(u) = user {
            args.extend(["--user".into(), u.into()]);
        }
        let log = run::start_log()?;
        let command = format!("request system secret {} {name}", act.verb());
        match act {
            Act::Set | Act::SetFile(_) => {
                // Non-dumpable while it is held, as a password is (ADR-040).
                rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable).context("protecting the value")?;
                let value = if let Act::SetFile(path) = act {
                    // Read as the person: a file of theirs (an SSH key), no larger than the broker keeps.
                    use std::io::Read as _;
                    let mut s = String::new();
                    std::io::Read::take(fs::File::open(path).with_context(|| format!("reading {path}"))?, crate::secrets::VALUE_MAX as u64 + 1).read_to_string(&mut s).with_context(|| format!("reading {path} (text: an SSH key, a token)"))?;
                    if s.len() > crate::secrets::VALUE_MAX {
                        bail!("not changed: {path} is larger than a secret is (16 KiB)");
                    }
                    s
                } else {
                    if !std::io::stdin().is_terminal() {
                        bail!("a secret is typed at a terminal: `{command}` there (or `… file <path>`)");
                    }
                    let typed = |q: &str| -> Result<String> {
                        print!("{q}: ");
                        std::io::stdout().flush()?;
                        let _hidden = Echo::set(false);
                        let mut s = String::new();
                        std::io::stdin().lock().read_line(&mut s)?;
                        println!();
                        Ok(s.trim_end_matches(['\n', '\r']).to_string())
                    };
                    let value = typed(&format!("Value of {whose} (not shown as you type)"))?;
                    if typed("Again")? != value {
                        bail!("not changed: the two differ");
                    }
                    value
                };
                if value.trim().is_empty() {
                    bail!("not changed: no value");
                }
                // In memory only, never on a disk (a killed torqcli would leave it there), and gone when this is.
                let scratch = crate::memory_scratch_dir()?;
                let file = scratch.path().join("value");
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?.write_all(value.as_bytes())?;
                }
                // As the broker reads it run directly (root's torqcli); over the socket, sent as the request's own.
                args.extend(["--value-file".into(), file.to_string_lossy().into_owned(), "--approved-by".into(), me.into()]);
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let v = done(run::broker_json(broker(self.cli, &a)?, &log)?, &command)?;
                Ok(format!(
                    "The value of {whose} is kept (ledger #{}): encrypted, root's alone, outside the configuration. What it may be used for: request system secret allow {name} ….\n",
                    v["seq"].as_u64().unwrap_or(0)
                ))
            }
            Act::Show => {
                args.extend(["--approved-by".into(), me.into()]);
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let v = done(run::broker_json(broker(self.cli, &a)?, &log)?, &command)?;
                Ok(format!("{}\n", v["value"].as_str().unwrap_or_default()))
            }
            Act::Delete => {
                args.extend(["--approved-by".into(), me.into()]);
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                let v = done(run::broker_json(broker(self.cli, &a)?, &log)?, &command)?;
                Ok(format!("The value of {whose} is deleted (ledger #{}).\n", v["seq"].as_u64().unwrap_or(0)))
            }
            _ => bail!("not a value's request"),
        }
    }

    fn known_host_keys(&self, host: &str) -> Result<Vec<String>> {
        // ssh-keygen finds the host's lines in the person's own known_hosts, hashed or not; each is written back
        // as the host in the clear, its type and key. A marked line (@revoked, @cert-authority) is not a key to trust.
        let out = std::process::Command::new("ssh-keygen").args(["-F", host]).stderr(std::process::Stdio::null()).output().context("running ssh-keygen (OpenSSH)")?;
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#') && !l.starts_with('@'))
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                (f.len() >= 3).then(|| format!("{host} {} {}", f[1], f[2]))
            })
            .filter(|l| torq_wire::secrets::known_ok(l, &[host.to_string()]))
            .collect())
    }

    fn secrets_kept(&self) -> Result<Vec<crate::secrets::Kept>> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["secrets"])?, &log)?, "show system secrets")?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| {
                let name = k["name"].as_str()?.to_string();
                match &k["owner"] {
                    serde_json::Value::String(s) if s == "system" => Some((None, name)),
                    o => Some((Some(o["person"].as_str()?.to_string()), name)),
                }
            })
            .collect())
    }

    fn backup_key(&self, name: &str, me: &str) -> Result<String> {
        if !std::io::stdin().is_terminal() {
            bail!("a key is typed at a terminal: `request system backup key {name}` there");
        }
        let typed = |q: &str| -> Result<String> {
            print!("{q}: ");
            std::io::stdout().flush()?;
            let _hidden = Echo::set(false);
            let mut s = String::new();
            std::io::stdin().lock().read_line(&mut s)?;
            println!();
            Ok(s.trim_end_matches(['\n', '\r']).to_string())
        };
        // Non-dumpable while it is held, as a password is (ADR-040).
        rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable).context("protecting the key")?;
        let key = typed(&format!("Key of backup {name} (its repository's passphrase)"))?;
        if key.is_empty() {
            bail!("not changed: no key");
        }
        if typed("Again")? != key {
            bail!("not changed: the two differ");
        }
        // To the broker as a password's hash goes: a private file, gone as soon as it is read.
        let scratch = crate::scratch_dir()?;
        let file = scratch.path().join("key");
        {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?.write_all(key.as_bytes())?;
        }
        let log = run::start_log()?;
        let file = file.to_string_lossy().into_owned();
        let v = done(run::broker_json(broker(self.cli, &["backup-key", "--name", name, "--key-file", &file, "--approved-by", me])?, &log)?, "request system backup key")?;
        Ok(format!(
            "The key of backup {name} is kept (ledger #{}): root's alone, outside the configuration. Keep it somewhere else too: without it, nothing in the backup can be read.\n",
            v["seq"].as_u64().unwrap_or(0)
        ))
    }

    fn restore_backup(&self, name: &str, snapshot: Option<&str>, path: &str, me: &str) -> Result<String> {
        let log = run::start_log()?;
        let mut args = vec!["backup-restore", "--name", name, "--path", path, "--approved-by", me];
        if let Some(s) = snapshot {
            args.extend(["--snapshot", s]);
        }
        let v = done(run::broker_json(broker(self.cli, &args)?, &log)?, "request system restore")?;
        let kept = v["data"]["set_aside"].as_str().map_or(String::new(), |a| format!(" What was there is at {}.", history::one_line(a, 120)));
        Ok(format!(
            "{} is back from backup {name}, snapshot {} (ledger #{}).{kept} A service using it should be restarted.\n",
            history::one_line(path, 120),
            history::one_line(&v["data"]["backup_snapshot"].as_str().unwrap_or("?").chars().take(8).collect::<String>(), 8),
            v["seq"].as_u64().unwrap_or(0)
        ))
    }

    fn remote_key(&self) -> Result<String> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["remote-key"])?, &log)?, "request system remote key")?;
        Ok(format!(
            "{}\n\nAdd this key to {} as a deploy key, with write access (and protect its branches: no force-push).\n",
            history::one_line(v["public_key"].as_str().unwrap_or("?"), 200),
            history::one_line(v["url"].as_str().unwrap_or("the remote"), 120)
        ))
    }

    fn propose(&self, diff: &str, base: &str, reason: &str) -> Result<(u64, Option<u64>)> {
        let scratch = scratch_dir()?;
        let file = scratch.path().join("change.diff");
        fs::write(&file, diff)?;
        let log = run::start_log()?;
        let file = file.to_string_lossy();
        let mut args = propose_args(&file, base, reason);
        if let Some(s) = agent_session(std::env::var("TORQ_AGENT_SESSION").ok()) {
            args.push(format!("--session={s}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let proposed = done(run::broker_json(broker(self.cli, &args)?, &log)?, "propose")?;
        let seq = proposed["seq"].as_u64().context("the broker did not say which proposal it made")?;
        // Applied at once under a person's trust (ADR-042, 7), if the broker says so.
        Ok((seq, proposed["trust_applied"]["seq"].as_u64()))
    }

    fn proposals(&self, fit: Option<usize>) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        let waiting = history::waiting_proposals(&entries);
        if waiting.is_empty() {
            return Ok("No proposals are waiting.\n".into());
        }
        let width = fit.unwrap_or(usize::MAX);
        let mut out = String::from("Waiting for a person (request system apply <n>):\n");
        for e in waiting {
            let by = e.data["caller"].as_str().map_or(e.actor.clone(), |c| format!("{} ({})", e.actor, c));
            let line = format!(
                "  #{:<4} {}  {:<6} {}{}  {}",
                e.seq,
                history::one_line(&e.ts, 25),
                history::one_line(&e.tier, 8),
                history::one_line(&by, 30),
                if e.tainted { "  ⚠ read untrusted content" } else { "" },
                history::one_line(&e.reason, 80)
            );
            out.push_str(&format!("{}\n", history::one_line(&line, width)));
        }
        Ok(out)
    }

    fn apply_proposal(&self, seq: u64, me: &str) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        if !history::waiting_proposals(&entries).iter().any(|e| e.seq == seq) {
            bail!("#{seq} is not a proposal waiting to be applied (show system proposals lists them)");
        }
        let log = run::start_log()?;
        let seq_s = seq.to_string();
        let p = done(run::broker_json(broker(self.cli, &["show", &seq_s])?, &log)?, "apply")?;
        // The entry as the ledger has it: its fields at the top (the broker flattens its body).
        let body = &p;
        let tainted = body["tainted"].as_bool().unwrap_or(false);
        let files: Vec<String> = body["data"]["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).map(|f| history::one_line(f, 80)).collect();
        let by = body["data"]["caller"].as_str().or(body["actor"].as_str()).unwrap_or("?");
        // What the change does, as the broker had git write it when it was proposed — never the
        // proposer's own text, which can say one thing while git does another (the audit of ADR-041).
        let effect = match (body["data"]["effect"].as_str(), body["actor"].as_str()) {
            (Some(e), _) => e,
            (None, Some("agent")) => bail!("not applied: #{seq} was proposed before the broker recorded what a change does; the agent proposes it again"),
            (None, _) => body["data"]["diff"].as_str().unwrap_or_default(),
        };
        let diff: String = effect.lines().map(|l| format!("  {}\n", history::clean(l))).collect();
        let what = format!(
            "Proposal #{seq} \"{}\" (by {}, {})\n  files: {}\n{diff}",
            history::one_line(body["reason"].as_str().unwrap_or_default(), 60),
            history::one_line(by, 30),
            history::one_line(body["ts"].as_str().unwrap_or_default(), 25),
            files.join(", "),
        );
        let (why, confirm) = apply_warning(body);
        if let Some(stop) = self.ask(&format!("{what}{why}Apply? ")).go_on("applied")? {
            return Ok(stop);
        }
        let minutes = confirm.map(|m| m.to_string());
        let mut args = vec!["apply", &seq_s, "--approved-by", me];
        if tainted {
            args.push("--confirm-tainted"); // asked just now, in words
        }
        if let Some(m) = &minutes {
            args.extend(["--confirm", m]);
        }
        let v = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Applying")) };
            done(run::broker_json(broker(self.cli, &args)?, &log)?, "apply")?
        };
        Ok(match confirm {
            Some(m) => format!("applied proposal #{seq} (change #{}) as a commit confirmed: it rolls back in {m} minutes unless you confirm it (configure; commit)\n", v["seq"].as_u64().unwrap_or(0)),
            None => format!("applied proposal #{seq} (change #{})\n", v["seq"].as_u64().unwrap_or(0)),
        })
    }

    fn trust(&self, minutes: Option<u32>) -> Result<String> {
        let log = run::start_log()?;
        let me = me()?;
        let Some(m) = minutes else {
            let v = done(run::broker_json(broker(self.cli, &["trust-revoke", "--approved-by", &me])?, &log)?, "request system agent trust revoke")?;
            return Ok(if v["revoked"].is_null() { "You had given the agent no trust to revoke.\n".into() } else { "Your trust in the agent is ended: its proposals wait for you again.\n".into() });
        };
        let m_s = m.to_string();
        let v = done(run::broker_json(broker(self.cli, &["trust-grant", "--minutes", &m_s, "--approved-by", &me])?, &log)?, "request system agent trust")?;
        Ok(format!(
            "Until {} ({m} minute{}), the agent's proposals in your own folder apply without asking (change #{}): never a system change, never one from a session that read untrusted text. Each is its own change in the ledger (request system revert <n> undoes it); request system agent trust revoke ends it now.\n",
            history::one_line(v["data"]["until"].as_str().unwrap_or("?"), 40),
            if m == 1 { "" } else { "s" },
            v["seq"].as_u64().unwrap_or(0)
        ))
    }

    fn commit(&self, diff: &str, base: &str, tier: &str, reason: &str, confirm: Option<u32>) -> Result<u64> {
        let scratch = scratch_dir()?;
        let file = scratch.path().join("change.diff");
        fs::write(&file, diff)?;
        let log = run::start_log()?;
        let file = file.to_string_lossy();
        let args = propose_args(&file, base, reason);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let live = self.detail.get();
        let proposed = crate::session::step(self, "Proposing the change to the broker", || done(run::broker_json_live(broker(self.cli, &args)?, &log, live)?, "commit"))?;
        let seq = proposed["seq"].as_u64().context("the broker did not say which proposal it made")?;
        // What was shown (and, for the system, asked) must be what the broker applies.
        if proposed["tier"].as_str() != Some(tier) {
            bail!(
                "not committed: the broker treats this as a {} change, not a {tier} one (recorded as proposal {seq}, not applied)",
                history::one_line(proposed["tier"].as_str().unwrap_or("?"), 20)
            );
        }
        let (seq_s, minutes) = (seq.to_string(), confirm.map(|m| m.to_string()));
        let mut args = vec!["apply", &seq_s, "--approved-by"];
        let who = me()?;
        args.push(&who);
        if let Some(m) = &minutes {
            args.extend(["--confirm", m]);
        }
        let outcome = {
            let _p = if crate::asks_for_password(self.cli) || live { None } else { Some(run::Progress::start("Committing")) };
            crate::session::step(self, "The broker applies it: it builds the system and switches to it", || run::broker_json_live(broker(self.cli, &args)?, &log, live))
        };
        match outcome.and_then(|o| done(o, "commit")) {
            Ok(applied) => applied["seq"].as_u64().context("the broker did not say which change it applied"),
            // The answer was lost, or unclear: the ledger says whether the change was made. (A
            // refusal is certain, and keeps the candidate.)
            Err(e) => match crate::read_ledger(self.cli).ok().and_then(|l| {
                l.iter().find(|x| x.kind == "applied" && x.data["proposal"].as_u64() == Some(seq)).map(|x| x.seq)
            }) {
                Some(applied) => Ok(applied),
                None => Err(e),
            },
        }
    }

    fn commit_time(&self, text: &str) -> Result<(String, String)> {
        let clock = history::Clock::system();
        let t = crate::at::when(text, clock.now, &crate::at::system_today()?, &crate::at::system_local)?;
        let utc = crate::at::rfc3339(t);
        let local = history::local_stamp(&utc, &history::Clock { now: clock.now, offset: crate::at::system_local_offset(t).unwrap_or(clock.offset) });
        Ok((utc, local))
    }

    fn commit_at(&self, diff: &str, base: &str, tier: &str, reason: &str, at: &str) -> Result<u64> {
        let scratch = scratch_dir()?;
        let file = scratch.path().join("change.diff");
        fs::write(&file, diff)?;
        let log = run::start_log()?;
        let file = file.to_string_lossy();
        let args = propose_args(&file, base, reason);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let proposed = done(run::broker_json(broker(self.cli, &args)?, &log)?, "commit at")?;
        let seq = proposed["seq"].as_u64().context("the broker did not say which proposal it made")?;
        // What was shown (and, for the system, asked) must be what the broker schedules.
        if proposed["tier"].as_str() != Some(tier) {
            bail!(
                "not scheduled: the broker treats this as a {} change, not a {tier} one (recorded as proposal {seq}, not applied)",
                history::one_line(proposed["tier"].as_str().unwrap_or("?"), 20)
            );
        }
        let (seq_s, at_arg, who) = (seq.to_string(), format!("--at={at}"), me()?);
        let v = done(run::broker_json(broker(self.cli, &["schedule", &seq_s, &at_arg, "--approved-by", &who])?, &log)?, "commit at")?;
        v["seq"].as_u64().context("the broker did not say what it recorded")
    }

    fn scheduled(&self) -> Result<Vec<crate::session::Scheduled>> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["scheduled"])?, &log)?, "the commits scheduled")?;
        let clock = history::Clock::system();
        Ok(v["scheduled"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                let at = s["at"].as_str()?;
                let offset = history::parse_ts(at).and_then(crate::at::system_local_offset).unwrap_or(clock.offset);
                Some(crate::session::Scheduled {
                    seq: s["seq"].as_u64()?,
                    proposal: s["proposal"].as_u64()?,
                    when: history::local_stamp(at, &history::Clock { now: clock.now, offset }),
                    by: s["approved_by"].as_str().map(|b| history::one_line(b, 40)),
                    reason: history::one_line(s["reason"].as_str().unwrap_or_default(), 200),
                    late: history::parse_ts(at).is_some_and(|t| clock.now > t + crate::at::LATE_SECONDS),
                })
            })
            .collect())
    }

    fn cancel_scheduled(&self, seq: u64, me: &str) -> Result<u64> {
        let log = run::start_log()?;
        let seq_s = seq.to_string();
        let v = done(run::broker_json(broker(self.cli, &["cancel-scheduled", &seq_s, "--approved-by", me])?, &log)?, "clear system commit")?;
        v["seq"].as_u64().context("the broker did not say what it recorded")
    }

    fn pending_confirm(&self) -> Result<Option<Waiting>> {
        let log = run::start_log()?;
        let v = match run::broker_status(broker(self.cli, &["status"])?, &log)? {
            Ok(v) => v,
            Err(why) => bail!("cannot check for a commit confirmed waiting: {why}"),
        };
        let p = &v["pending_confirm"];
        Ok(p["applied"].as_u64().map(|seq| Waiting {
            seq,
            deadline: history::one_line(p["deadline"].as_str().unwrap_or("?"), 40),
            by: p["approved_by"].as_str().map(|s| history::one_line(s, 40)),
        }))
    }

    fn confirm(&self) -> Result<u64> {
        let log = run::start_log()?;
        let v = done(run::broker_json(broker(self.cli, &["confirm", "--approved-by", &me()?])?, &log)?, "confirm")?;
        v["seq"].as_u64().context("the broker did not say what it recorded")
    }

    fn alarms(&self, detail: bool, fit: Option<usize>) -> Result<String> {
        let (v, report) = self.status_report(fit)?;
        let mut out = report.text;
        if let Some(seq) = v["pending_confirm"]["applied"].as_u64() {
            out.push_str(&format!(
                "  change #{seq} (commit confirmed) rolls back at {} unless confirmed\n",
                history::one_line(v["pending_confirm"]["deadline"].as_str().unwrap_or("?"), 40)
            ));
        }
        out.push_str(&trust_lines(&v["trust"]));
        if detail {
            // The facts behind the report, as the broker gives them (`torq-broker status`).
            let fact = |k: &str| match &v[k] {
                serde_json::Value::Null => "-".to_string(),
                serde_json::Value::String(s) => history::one_line(s, 120),
                other => history::one_line(&other.to_string(), 120),
            };
            use crate::ui::{Col, Table};
            // Data (commits, store paths): never wrapped.
            let mut t = Table::new(&[Col::Left(""), Col::Left("")]).indent(2);
            for (label, key) in [
                ("config repo commit", "head"),
                ("ledgered commit", "ledgered_commit"),
                ("ledger entries", "ledger_entries"),
                ("ledgered system", "ledgered_system"),
                ("running system", "running_system"),
                ("next boot", "next_boot"),
            ] {
                t.row(vec![label.to_string(), fact(key)]);
            }
            t.row(vec!["from".to_string(), format!("{} status", history::one_line(&self.cli.broker, 80))]);
            out.push_str(&t.render(&crate::ui::Ui::plain_at(None)));
        }
        Ok(out)
    }

    fn search(&self, terms: &[String], installed: &std::collections::BTreeSet<String>, fit: Option<usize>) -> Result<String> {
        if let Some(t) = terms.iter().find(|t| t.starts_with('-')) {
            bail!("'{t}' is not a word to search for (names and descriptions are searched)");
        }
        let log = run::start_log()?;
        let found = {
            let _p = run::Progress::start("Searching nixpkgs");
            crate::search(&self.cli.repo, terms, &log)?
        };
        if found.is_empty() {
            return Ok(format!("No package matches {}.\n", terms.join(" ")));
        }
        // A table: name and version, then the description as a sentence beside them (ui.rs); `*`: in
        // the configuration already, as on a network device.
        use crate::ui::{Col, Table};
        let mut t = Table::new(&[Col::Left(""), Col::Left(""), Col::Text("")]).widest(0, 32).widest(1, 16);
        for (n, v, d) in &found {
            let mark = if installed.contains(n) { "* " } else { "  " };
            t.row(vec![format!("{mark}{n}"), v.clone(), d.clone()]);
        }
        let mut out = t.render(&crate::ui::Ui::plain_at(fit));
        if found.iter().any(|(n, _, _)| installed.contains(n)) {
            out.push_str("* installed by this configuration\n");
        }
        Ok(out)
    }

    /// Only when the report says a repair can fix what is wrong, and only
    /// with a person's yes — it rebuilds and switches the whole system.
    fn set_password(&self, user: &str) -> Result<String> {
        if !std::io::stdin().is_terminal() {
            bail!("a password is typed at a terminal: `request system password` there");
        }
        if !crate::pkg::valid_user(user) {
            bail!("'{user}' is not a user name");
        }
        let typed = |q: &str| -> Result<String> {
            print!("{q}: ");
            std::io::stdout().flush()?;
            let _hidden = Echo::set(false);
            let mut s = String::new();
            std::io::stdin().lock().read_line(&mut s)?;
            println!();
            Ok(s.trim_end_matches(['\n', '\r']).to_string())
        };
        let new = typed(&format!("New password for {user}"))?;
        if new.chars().count() < 8 {
            bail!("not changed: at least 8 characters");
        }
        if typed("Again")? != new {
            bail!("not changed: the two differ");
        }
        let hash = crate::setup::hash(&new)?;
        // To the broker as a diff goes: a private file, gone as soon as it is read.
        let scratch = crate::scratch_dir()?;
        let file = scratch.path().join("hash");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?;
            writeln!(f, "{hash}")?;
        }
        let log = run::start_log()?;
        let file = file.to_string_lossy().into_owned();
        let v = done(run::broker_json(broker(self.cli, &["password", "--user", user, "--hash-file", &file, "--approved-by", &me()?])?, &log)?, "password")?;
        Ok(format!("password changed for {user} (ledger #{})\n", v["seq"].as_u64().unwrap_or(0)))
    }

    fn repair(&self) -> Result<String> {
        // Shown with a question, at the terminal: fitting it.
        let (_, report) = self.status_report(crate::ui::fit(crate::ui::columns(false)))?;
        match report.fix {
            crate::status::Fix::Repair => {}
            crate::status::Fix::Nothing if report.exit == 0 => return Ok(format!("{}  Nothing to repair.\n", report.text)),
            _ => bail!("not repaired: a repair would not fix this (see `show system alarms`); a person has to look"),
        }
        let shown: String = report.text.lines().map(|l| format!("{}\n", history::clean(l))).collect();
        if let Some(stop) = self.ask(&format!("{shown}This rebuilds and switches the system (all users). Repair? ")).go_on("repaired")? {
            return Ok(stop);
        }
        let log = run::start_log()?;
        let v = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Repairing")) };
            done(run::broker_json(broker(self.cli, &["repair", "--approved-by", &me()?])?, &log)?, "repair")?
        };
        Ok(format!("repaired: the machine is back at its last ledgered configuration (#{})\n", v["seq"].as_u64().unwrap_or(0)))
    }

    /// As `torq service enable ssh` did: the catalog's SSH lets in the owner's keys only, so these —
    /// as the changed configuration will have them — are exactly what approving it approves.
    fn ssh_keys(&self, facts: &SshFacts) -> Result<(String, Vec<String>)> {
        let (owner, keys) = crate::admitted(&facts.admits)?;
        let mut rows = Vec::new();
        for (note, id, place) in keys {
            if !note.is_empty() {
                rows.push(format!("  ! {}", history::one_line(note.trim_end_matches(": "), 120)));
            }
            // Never truncated: a key is approved by what it is.
            rows.push(format!("    {}  (from {})", history::clean(&id), history::one_line(&place, 80)));
        }
        Ok((owner, rows))
    }

    fn ledger(&self, count: usize, fit: Option<usize>) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        Ok(history::render(&entries, count, &crate::ui::Ui::plain_at(fit), &history::Clock::system()))
    }

    /// The running machine, from the kernel (/proc, /sys), `ip -j addr`, `df`, and its system profiles.
    fn facts(&self) -> Result<crate::facts::Facts> {
        let read = |p: &str| fs::read_to_string(p).map(|s| s.trim().to_string()).with_context(|| format!("reading {p}"));
        let uptime = read("/proc/uptime")?.split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) as u64;
        let loads: Vec<f64> = read("/proc/loadavg")?.split_whitespace().take(3).filter_map(|s| s.parse().ok()).collect();
        let load = [loads.first().copied().unwrap_or(0.0), loads.get(1).copied().unwrap_or(0.0), loads.get(2).copied().unwrap_or(0.0)];
        // Interfaces, as iproute2 reports them (its JSON, not its text).
        let ip = std::process::Command::new("/run/current-system/sw/bin/ip").args(["-j", "addr"]).output().context("running ip")?;
        let list: serde_json::Value = serde_json::from_slice(&ip.stdout).context("reading ip's answer")?;
        let interfaces = list
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| crate::facts::Interface {
                name: i["ifname"].as_str().unwrap_or_default().to_string(),
                state: i["operstate"].as_str().unwrap_or("unknown").to_lowercase(),
                mac: i["address"].as_str().unwrap_or_default().to_string(),
                addresses: i["addr_info"].as_array().into_iter().flatten().filter_map(|a| Some(format!("{}/{}", a["local"].as_str()?, a["prefixlen"].as_u64()?))).collect(),
            })
            .collect();
        // Disks, not the kernel's own filesystems in memory.
        let df = std::process::Command::new("/run/current-system/sw/bin/df")
            .args(["-P", "-B1", "-x", "tmpfs", "-x", "devtmpfs", "-x", "efivarfs", "-x", "overlay", "-x", "ramfs"])
            .output()
            .context("running df")?;
        let filesystems = String::from_utf8_lossy(&df.stdout)
            .lines()
            .skip(1)
            .filter_map(|l| {
                let f: Vec<&str> = l.split_whitespace().collect();
                Some(crate::facts::Filesystem { device: f.first()?.to_string(), size: f.get(1)?.parse().ok()?, used: f.get(2)?.parse().ok()?, free: f.get(3)?.parse().ok()?, mount: f.get(5)?.to_string() })
            })
            .collect();
        let generations = fs::read_dir("/nix/var/nix/profiles").map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("system-") && e.file_name().to_string_lossy().ends_with("-link")).count()).unwrap_or(0);
        // People and where they are logged in: the account files, and systemd's own record of logins.
        let mut people = crate::facts::people(&read("/etc/passwd")?, &read("/etc/group")?);
        let loginctl = "/run/current-system/sw/bin/loginctl";
        let list = std::process::Command::new(loginctl).args(["list-sessions", "--json=short"]).output().context("running loginctl")?;
        let ids: Vec<String> = serde_json::from_slice::<serde_json::Value>(&list.stdout)
            .context("reading loginctl's answer")?
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["session"].as_str().map(String::from))
            .collect();
        if !ids.is_empty() {
            let show = std::process::Command::new(loginctl).arg("show-session").args(&ids).args(["-p", "Name", "-p", "Class", "-p", "TTY", "-p", "RemoteHost"]).output().context("running loginctl")?;
            for (name, at) in crate::facts::sessions(&String::from_utf8_lossy(&show.stdout)) {
                if let Some(p) = people.iter_mut().find(|p| p.name == name) {
                    p.sessions.push(at);
                }
            }
        }
        Ok(crate::facts::Facts { nixos: read("/run/current-system/nixos-version")?, kernel: read("/proc/sys/kernel/osrelease")?, uptime, load, interfaces, filesystems, generations, people })
    }

    /// As `torq undo` did, with a commit's rule for asking: a change only to your own folder goes
    /// ahead; anything else (the system, someone else's folder) needs a person at a terminal.
    fn revert(&self, seq: u64, me: &str) -> Result<String> {
        let entries = crate::read_ledger(self.cli)?;
        let target = history::undo_target(&entries, Some(seq))?;
        let files: Vec<String> =
            target.data["files"].as_array().map(|f| f.iter().filter_map(|v| v.as_str()).map(|s| history::one_line(s, 80)).collect()).unwrap_or_default();
        let mine = only_own_folder(&target.tier, &files, me);
        let what = format!(
            "Revert change #{} \"{}\" ({}, by {}{})\n  files: {}\n",
            target.seq,
            history::one_line(&target.reason, 60),
            history::one_line(&target.ts, 25),
            history::one_line(target.approved_by.as_deref().unwrap_or(&target.actor), 20),
            if target.tainted { ", from a session that read untrusted content" } else { "" },
            if files.is_empty() { "-".to_string() } else { files.join(", ") }
        );
        let what = format!("{what}{}", self.revert_ssh_keys(target)?);
        if !mine {
            let why = if target.tier == "user" { "This changes someone else's settings." } else { "This changes the system (all users)." };
            if let Some(stop) = self.ask(&format!("{what}{why} Revert? ")).go_on("reverted")? {
                return Ok(stop);
            }
        }
        let log = run::start_log()?;
        let outcome = {
            let _p = if crate::asks_for_password(self.cli) { None } else { Some(run::Progress::start("Reverting")) };
            run::broker_json(broker(self.cli, &["revert", &target.seq.to_string(), &format!("--approved-by={me}")])?, &log)
        };
        match outcome {
            Ok(run::Outcome::Refused(m)) if m.contains("later changes conflict") => {
                // Name what stands in the way, from the ledger, rather than git's view of it.
                let conflicts = history::later_conflicts(&entries, target.seq);
                let later: Vec<String> = conflicts.iter().map(|s| format!("#{s}")).collect();
                // A revert cannot itself be reverted: then only the opposite change helps.
                let reverted = conflicts.iter().any(|s| entries.iter().any(|e| e.seq == *s && e.kind == "reverted"));
                let advice = if reverted {
                    "make the opposite change in configuration mode"
                } else {
                    "revert those first (newest first), or make the opposite change in configuration mode"
                };
                bail!(
                    "change #{} cannot be reverted on its own: later changes ({}) changed the same files; {advice}",
                    target.seq,
                    if later.is_empty() { "?".to_string() } else { later.join(", ") }
                )
            }
            o => match o.and_then(|o| done(o, "revert")) {
                Ok(v) => Ok(format!("{what}reverted: change #{} undone (change #{})\n", target.seq, v["seq"].as_u64().unwrap_or(0))),
                // The answer was lost, or unclear: the ledger says whether it was reverted.
                Err(e) => match crate::read_ledger(self.cli).ok().and_then(|l| {
                    l.iter().find(|x| x.kind == "reverted" && x.data["applied"].as_u64() == Some(target.seq)).map(|x| x.seq)
                }) {
                    Some(n) => Ok(format!("{what}reverted: change #{} undone (change #{n}; the broker's answer was lost)\n", target.seq)),
                    None => Err(e),
                },
            },
        }
    }

    /// The ledger's entries that produced a configuration (as the broker's `ledgered_commit` counts
    /// them), newest first. The ledger is the broker's: read through it, as a commit is.
    fn configurations(&self, _repo: &Repo, count: usize) -> Result<History> {
        let entries = crate::read_ledger(self.cli)?;
        Ok(History { configurations: configurations_of(&entries, count, &history::Clock::system()), at: ledgered_commit(&entries) })
    }

    /// As `torq update` does it, but on the committed configuration the candidate is based on (as
    /// the broker builds it), not the repo's working copy; nothing is proposed here.
    fn update_lock(&self, base: &str, all: bool) -> Result<String> {
        self.relock(base, None, all)
    }

    fn lock_with(&self, base: &str, flake_nix: &str, all: bool) -> Result<String> {
        self.relock(base, Some(flake_nix), all)
    }
    fn releases(&self) -> Result<String> {
        let file = "/etc/torq/releases.json";
        let text = fs::read_to_string(file).with_context(|| format!("reading {file}"))?;
        let v: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("{file} is not JSON"))?;
        v["url"].as_str().map(String::from).with_context(|| format!("{file} names no releases (url)"))
    }
    fn check_release(&self, repo: &str, rev: &str) -> Result<crate::release::Signed> {
        let _p = self.progress("Checking the release's signature");
        crate::release::check(repo, rev, Path::new(crate::release::SIGNERS))
    }
    fn running_version(&self) -> Option<String> {
        fs::read_to_string("/etc/os-release").ok().and_then(|t| crate::release::os_release_version(&t))
    }
    fn ask(&self, question: &str) -> crate::session::Answer {
        if let Some(asker) = &self.asker {
            return asker(question);
        }
        ask_person(question)
    }
    fn read_terminal(&self, max: usize) -> Result<String> {
        if !std::io::stdin().is_terminal() {
            bail!("load … terminal reads what is pasted at a terminal; from a script, load a file");
        }
        // What was pasted before this asked for it is not the input (it was read as commands, or is
        // still waiting): it ends the batch, as a question does.
        ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
        println!("[Type ^D at a new line to end input]");
        let _seen = Echo::set(true);
        let mut text = Vec::new();
        let mut stdin = std::io::stdin().lock();
        std::io::Read::read_to_end(&mut std::io::Read::take(&mut stdin, max as u64 + 1), &mut text)?;
        if text.len() > max {
            // Too much: read the rest up to Ctrl-D too, so none of it runs as commands.
            std::io::copy(&mut stdin, &mut std::io::sink())?;
            bail!("more than {max} bytes: not a configuration");
        }
        String::from_utf8(text).context("not text")
    }
    fn edit(&self, path: &str, text: &str) -> Result<Option<String>> {
        // (The editor shows what is typed: echo is off while a command runs.)
        let _seen = Echo::set(true);
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!("file edit opens your editor: at a terminal (`file show {path}` prints it)");
        }
        // A private folder of this user's, named as the file, so the editor knows it is Nix.
        let scratch = scratch_dir()?;
        let name = Path::new(path).file_name().context("a config file has a name")?;
        let f = scratch.path().join(name);
        fs::write(&f, text)?;
        let editor = ["VISUAL", "EDITOR"].iter().find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()));
        let editor = editor.unwrap_or_else(|| if which("nano") { "nano".into() } else { "vi".into() });
        // As the shell would run $EDITOR (it may carry options: `code --wait`).
        let status = std::process::Command::new("sh").arg("-c").arg(format!("{editor} \"$1\"")).arg("sh").arg(&f).status()?;
        if !status.success() {
            bail!("{path}: the editor ({editor}) failed; nothing changed");
        }
        let now = fs::read_to_string(&f).with_context(|| format!("{path}: not text; nothing changed"))?;
        Ok((now != text).then_some(now))
    }
    /// Nix's parser, on its own (`--parse`: nothing evaluated, nothing read or built).
    fn nix_syntax(&self, path: &str, text: &str) -> Result<Option<String>> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("/run/current-system/sw/bin/nix-instantiate")
            .args(["--parse", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("running nix-instantiate --parse")?;
        child.stdin.take().context("nix-instantiate's input")?.write_all(text.as_bytes())?;
        let out = child.wait_with_output()?;
        if out.status.success() {
            return Ok(None);
        }
        // Nix's words, the file named as the person knows it, the lines it points at kept.
        let why = String::from_utf8_lossy(&out.stderr).replace("«stdin»", path);
        let lines: Vec<String> = why.lines().map(|l| l.strip_prefix("error: ").unwrap_or(l).trim_end().to_string()).filter(|l| !l.trim().is_empty()).collect();
        Ok(Some(lines.join("\n")))
    }

}

/// Refuse to write `path` under `root` if any folder on the way is a link (or the file itself is):
/// a committed `users/dev -> /home/dev/x` must not make the check write outside its scratch copy.
fn no_link_on_the_way(root: &Path, path: &str) -> Result<()> {
    let mut at = root.to_path_buf();
    for part in Path::new(path).components() {
        at.push(part);
        if fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{} is a link in the configuration; torqcli does not write through it", at.strip_prefix(root).unwrap_or(&at).display());
        }
    }
    Ok(())
}

#[cfg(test)]
#[test]
fn writes_never_go_through_a_link() {
    let d = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(d.path().join("users")).unwrap();
    std::os::unix::fs::symlink(outside.path(), d.path().join("users/dev")).unwrap();
    let e = format!("{:#}", no_link_on_the_way(d.path(), "users/dev/packages.json").unwrap_err());
    assert!(e.contains("users/dev is a link"), "{e}");
    std::os::unix::fs::symlink("/etc/passwd", d.path().join("services.json")).unwrap();
    assert!(no_link_on_the_way(d.path(), "services.json").is_err(), "the file itself too");
    assert!(no_link_on_the_way(d.path(), "system/host.json").is_ok());
}


/// The proposals no one has applied (or tried to): what `show system proposals` lists and
/// `request system apply` takes.
/// `propose`'s arguments. Every value given as `--name=value`: a reason or comment that starts with
/// `-` (any change of an existing value does: "-   host-name two; …") must not read as an option.
fn propose_args(diff: &str, base: &str, reason: &str) -> Vec<String> {
    vec!["propose".into(), format!("--diff={diff}"), format!("--base={base}"), format!("--reason={reason}"), "--actor=user".into()]
}

/// The agent's session at the broker, as the program that started it (`torq-doctor`) hands it to
/// torqcli: `TORQ_AGENT_SESSION`, an id and nothing else. The broker decides what it means; without
/// one, the agent's proposal is tainted (ADR-042, amended).
fn agent_session(var: Option<String>) -> Option<String> {
    var.filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// What a person is told before applying a proposal, and the minutes of the commit confirmed it is
/// applied as, if it could cut the machine off (ADR-042, 6).
fn apply_warning(body: &serde_json::Value) -> (String, Option<u32>) {
    // What its session read, as the broker recorded it in the proposal's own entry.
    let read: Option<Vec<String>> = body["data"]["session"]["tainted_by"].as_array().map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| history::one_line(s, 40)).collect());
    let mut why = String::new();
    if body["tainted"].as_bool().unwrap_or(false) {
        let from = match read.as_deref() {
            Some(r) if !r.is_empty() => format!("Before proposing this, the agent read text that anyone could have written ({}): a log line, a device's name, a web page can carry instructions meant for it. ", r.join(", ")),
            _ if body["actor"] == "agent" => "The broker cannot vouch for what the program that proposed this read (it named no session TorqOS's agent keeps): treat it as if it read the web. ".into(),
            _ => "The one who proposed it said it came from untrusted text (the web, mail, a download). ".into(),
        };
        why.push_str(&from);
        why.push_str("Judge the change itself, shown above, not the reason given for it. ");
    }
    let cuts: Vec<String> = body["data"]["cuts_off"].as_array().into_iter().flatten().filter_map(|s| s.as_str()).map(|s| history::one_line(s, 80)).collect();
    let confirm = (!cuts.is_empty()).then_some(CUTS_OFF_MINUTES);
    if body["tier"] != "user" {
        why.push_str("This changes the system (all users). ");
    }
    if !cuts.is_empty() {
        why.push_str(&format!(
            "It changes {}, which could cut this machine off the network or the screen: it is applied as a commit confirmed, and rolls itself back in {CUTS_OFF_MINUTES} minutes unless you confirm it (configure; commit). ",
            cuts.join(", ")
        ));
    }
    (why, confirm)
}

/// `show system alarms`: each person's trust in the agent while it holds (the broker's `status`,
/// `trust`) — not a problem, but what a person must see: changes may land without anyone asked.
fn trust_lines(trust: &serde_json::Value) -> String {
    trust
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            format!(
                "  the agent's proposals in {}'s folder apply without asking until {} (trust #{}; request system agent trust revoke ends it)\n",
                history::one_line(g["person"].as_str().unwrap_or("?"), 40),
                history::one_line(g["until"].as_str().unwrap_or("?"), 40),
                g["seq"].as_u64().unwrap_or(0)
            )
        })
        .collect()
}

#[test]
fn trust_is_shown_while_it_holds() {
    let t = serde_json::json!([{"seq": 12, "person": "viper", "minutes": 30, "until": "2026-11-07T10:30:00Z"}]);
    assert_eq!(trust_lines(&t), "  the agent's proposals in viper's folder apply without asking until 2026-11-07T10:30:00Z (trust #12; request system agent trust revoke ends it)\n");
    assert_eq!(trust_lines(&serde_json::Value::Null), "");
    // Text from the ledger never drives the terminal.
    assert!(!trust_lines(&serde_json::json!([{"seq": 1, "person": "v\u{1b}[2J", "until": "x"}])).contains('\u{1b}'));
}

/// How long an agent's change that could cut the machine off waits for a person's confirmation.
const CUTS_OFF_MINUTES: u32 = 10;

#[test]
fn a_person_is_told_why_before_applying() {
    use serde_json::json;
    let (w, c) = apply_warning(&json!({"tier": "user", "tainted": false, "actor": "agent", "data": {}}));
    assert_eq!((w.as_str(), c), ("", None));
    let (w, c) = apply_warning(&json!({"tier": "user", "tainted": true, "actor": "agent", "data": {"session": {"id": "s", "tainted_by": ["read_journal", "show_ledger"]}}}));
    assert!(w.contains("(read_journal, show_ledger)") && w.contains("not the reason") && c.is_none(), "{w}");
    let (w, _) = apply_warning(&json!({"tier": "user", "tainted": true, "actor": "agent", "data": {}}));
    assert!(w.contains("named no session"), "{w}");
    let (w, c) = apply_warning(&json!({"tier": "system", "tainted": false, "actor": "agent", "data": {"cuts_off": ["networking.wireless.enable"]}}));
    assert!(w.contains("networking.wireless.enable") && w.contains("commit confirmed"), "{w}");
    assert_eq!(c, Some(CUTS_OFF_MINUTES));
    let (w, _) = apply_warning(&json!({"tier": "system", "tainted": false, "actor": "user", "data": {}}));
    assert!(w.contains("all users"));
    assert_eq!(agent_session(Some("a".repeat(32))), Some("a".repeat(32)));
    for bad in ["A".repeat(32), "a".repeat(31), format!("{}; x", "a".repeat(29)), String::new()] {
        assert_eq!(agent_session(Some(bad.clone())), None, "{bad}");
    }
}

/// A `-c` script's commands: split at `;` and newlines, but not inside "double" or 'single' quotes
/// (as `words` reads them).
fn script_lines(script: &str) -> Vec<String> {
    let (mut out, mut cur, mut open, mut escaped) = (Vec::new(), String::new(), None, false);
    for c in script.chars() {
        match c {
            _ if escaped => {
                cur.push(c);
                escaped = false;
                continue;
            }
            '\\' if open == Some('"') => escaped = true,
            '"' | '\'' if open.is_none() => open = Some(c),
            _ if open == Some(c) => open = None,
            ';' | '\n' if open.is_none() => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

#[cfg(test)]
#[test]
fn propose_values_are_never_options() {
    let a = propose_args("/tmp/d", "abc", "-   host-name two; +   host-name three;");
    assert_eq!(a[3], "--reason=-   host-name two; +   host-name three;");
    assert!(a[1..].iter().all(|x| x.starts_with("--") && x.contains('=')), "{a:?}");
}

#[cfg(test)]
#[test]
fn scripts_split_outside_quotes() {
    assert_eq!(script_lines("configure; set a b\ncommit comment \"a; b\" ;"), ["configure", "set a b", "commit comment \"a; b\""]);
    assert_eq!(script_lines(r#"commit comment "say \"x; y\"""#), [r#"commit comment "say \"x; y\"""#]);
    assert_eq!(script_lines("show configuration | grep 'a;b'; show | cut -d';' -f1"), ["show configuration | grep 'a;b'", "show | cut -d';' -f1"]);
    assert_eq!(script_lines(r#"commit comment "it's; fine""#), [r#"commit comment "it's; fine""#], "a ' inside \" is text");
}

/// The shell: `torqcli` (interactive), `torqcli -c "<command>; <command>…"` (a private candidate,
/// stops at the first error) or `torqcli <command>` (one operational command). Returns the exit code.
pub fn main(args: &[String]) -> i32 {
    if matches!(args, [v] if v == "--version" || v == "-V") {
        say(&format!("torqcli {}\n", env!("CARGO_PKG_VERSION")));
        return 0;
    }
    let _ = VERBOSE.set(verbose_flag(args));
    match run_shell(args) {
        Ok(code) => code,
        Err(e) => {
            say_error(&describe(&e));
            1
        }
    }
}

/// How torqcli was asked to run.
#[derive(Debug, PartialEq)]
enum Invocation {
    /// The shell.
    Interactive,
    /// `--help`: how torqcli is run, on standard output.
    Help,
    /// `--candidate -c "<command>; …"`: a script on the person's own saved candidate, as the shell's, kept when
    /// it ends: how TorqOS Settings hands a commit of the draft to a terminal (ADR-045 2).
    Draft(String),
    /// `-c "<command>; <command>…"`.
    Script(String),
    /// `torqcli <command>`: one operational command, its words as the calling shell split them (one
    /// argument: a whole line, split here).
    Command(Vec<String>),
}

/// How torqcli is run, named as it was (`torq` or `torqcli`).
fn usage(name: &str) -> String {
    format!("usage: {name} [--verbose] [[--candidate] -c \"<command>; <command>…\" | <command>]")
}

/// The name torqcli was run by (`torq`, `torqcli`).
fn own_name() -> String {
    std::env::args().next().and_then(|a| Path::new(&a).file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "torqcli".into())
}

/// `--verbose`: errors in full (every cause, and where the whole log is) instead of one clear line.
static VERBOSE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether torqcli's own options (before the command: `-c …`) ask for `--verbose` — not
/// a `-v` of the command (`show configuration | grep -v x`).
fn verbose_flag(args: &[String]) -> bool {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-v" | "--verbose" => return true,
            "-c" => {
                it.next();
            }
            _ => return false,
        }
    }
    false
}

/// An error as torqcli says it: one clear line; with `--verbose`, each cause on its own line, then
/// the log the last broker or Nix command wrote (cleaned of anything that could fool a terminal).
fn describe(e: &anyhow::Error) -> String {
    if !VERBOSE.get().copied().unwrap_or(false) {
        return history::one_line(&e.chain().map(said).collect::<Vec<_>>().join(": "), 300);
    }
    let mut out = String::new();
    for (i, cause) in e.chain().enumerate() {
        let text = history::clean(&said(cause));
        if i == 0 { out.push_str(&text) } else { out.push_str(&format!("\n  because: {text}")) }
    }
    if let Ok(log) = run::log_path() {
        if log.exists() {
            out.push_str(&format!("\n  the whole log of the last broker or Nix command: {}", log.display()));
        }
    }
    out
}

/// Whether a run may ask a person what a command offers to go on with (Session::asks): the shell, or
/// one command, with standard input and output a terminal — never a script (`-c`), whatever it runs at.
fn asks(run: &Invocation, stdin_terminal: bool, stdout_terminal: bool) -> bool {
    !matches!(run, Invocation::Script(_) | Invocation::Draft(_)) && stdin_terminal && stdout_terminal
}

#[cfg(test)]
#[test]
fn a_script_is_never_asked() {
    let script = Invocation::Script("request system update; configure; commit".into());
    assert!(!asks(&script, true, true), "a -c script, even at a terminal");
    for run in [Invocation::Interactive, Invocation::Command(vec!["request".into(), "system".into(), "update".into()])] {
        assert!(asks(&run, true, true));
        assert!(!asks(&run, false, true) && !asks(&run, true, false), "input or output not a terminal");
    }
}

#[cfg(test)]
#[test]
fn a_script_on_the_persons_candidate_is_asked_for_by_name() {
    let a = |x: &[&str]| parse_args("torqcli", &x.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(a(&["--candidate", "-c", "configure; commit"]).unwrap(), Invocation::Draft("configure; commit".into()));
    assert_eq!(a(&["-c", "configure; commit", "--candidate"]).unwrap(), Invocation::Draft("configure; commit".into()), "in either order");
    assert_eq!(a(&["-c", "configure; commit"]).unwrap(), Invocation::Script("configure; commit".into()), "without it, a script's candidate stays its own");
    assert!(a(&["--candidate"]).is_err(), "it goes with -c");
    assert!(a(&["--candidate", "show", "version"]).is_err(), "it goes with -c");
    assert!(!asks(&Invocation::Draft("commit".into()), true, true), "a script is never asked to go on");
}

/// The command line (`name`: as torqcli was run). A refusal says why, then how torqcli is run.
fn parse_args(name: &str, args: &[String]) -> Result<Invocation> {
    // (One line, as every message: the usage after the reason.)
    let refuse = |why: String| anyhow::anyhow!("{why}; {}", usage(name));
    let mut run = Invocation::Interactive;
    let mut on_candidate = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // (Read before anything runs: see `describe`.)
            "-v" | "--verbose" => {}
            "-c" if run == Invocation::Interactive => {
                let Some(line) = it.next() else { return Err(refuse("-c needs the commands to run, as in -c \"show system alarms\"".into())) };
                run = Invocation::Script(line.clone());
            }
            "-c" => return Err(refuse("-c once: every command in one -c, separated by ;".into())),
            "--candidate" => on_candidate = true,
            "-h" | "--help" => return Ok(Invocation::Help),
            o if o.starts_with('-') => return Err(refuse(format!("'{o}' is not an option of {name} (--verbose, -c, --candidate, --help)"))),
            _ if run == Invocation::Interactive => {
                let rest: Vec<String> = std::iter::once(a).chain(it).cloned().collect();
                let w = if rest.len() == 1 { crate::session::words(&rest[0])? } else { rest };
                if matches!(w.first().map(String::as_str), Some("configure" | "config")) {
                    bail!("{name} <command> runs one operational command; to change the configuration: {name} -c \"configure; set …; commit\"");
                }
                if w.is_empty() {
                    return Err(refuse("an empty command".into()));
                }
                if on_candidate {
                    return Err(refuse("--candidate goes with -c".into()));
                }
                return Ok(Invocation::Command(w));
            }
            _ => return Err(refuse(format!("'{a}' after -c's commands: every command goes in -c, separated by ;"))),
        }
    }
    match run {
        Invocation::Script(line) if on_candidate => Ok(Invocation::Draft(line)),
        _ if on_candidate => Err(refuse("--candidate goes with -c".into())),
        run => Ok(run),
    }
}

#[cfg(test)]
#[test]
fn verbose_is_torqclis_own_option_only() {
    let v = |s: &str| verbose_flag(&s.split(' ').map(String::from).collect::<Vec<_>>());
    assert!(v("--verbose show system alarms") && v("-v -c configure") && v("-c configure -v"));
    assert!(!v("show configuration | grep -v x") && !v("-c configure") && !v("show -v"));
}

#[cfg(test)]
#[test]
fn a_command_on_the_command_line() {
    let a = |s: &[&str]| parse_args("torq", &s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
    let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(a(&[]).unwrap(), Invocation::Interactive);
    assert_eq!(a(&["show", "system", "alarms"]).unwrap(), Invocation::Command(w(&["show", "system", "alarms"])));
    assert_eq!(a(&["show configuration | grep 'a b'"]).unwrap(), Invocation::Command(w(&["show", "configuration", "|", "grep", "a b"])), "one argument: a line");
    assert_eq!(a(&["show", "configuration", "|", "grep", "a b"]).unwrap(), Invocation::Command(w(&["show", "configuration", "|", "grep", "a b"])), "words as given");
    assert_eq!(a(&["show", "-v"]).unwrap(), Invocation::Command(w(&["show", "-v"])), "options come first");
    assert_eq!(a(&["-c", "show; show"]).unwrap(), Invocation::Script("show; show".into()));
    assert_eq!(a(&["--help"]).unwrap(), Invocation::Help, "help is an answer, not an error");
    for bad in [&["-c", "x", "show"][..], &["-c", "x", "-c", "y"], &["-x"], &["-c"], &["--style", "j", "show"], &[""]] {
        assert!(a(bad).is_err(), "{bad:?}");
    }
    let e = format!("{:#}", a(&["configure"]).unwrap_err());
    assert!(e.contains("torq -c"), "named as it was run: {e}");
    for (bad, why) in [
        (&["-c"][..], "-c needs the commands to run, as in -c \"show system alarms\"; usage: torq [--verbose]"),
        (&["-c", "x", "-c", "y"], "-c once: every command in one -c, separated by ;"),
        (&["-c", "x", "show"], "'show' after -c's commands: every command goes in -c"),
        (&["-x"], "'-x' is not an option of torq (--verbose, -c, --candidate, --help)"),
        (&[""], "an empty command"),
    ] {
        let e = format!("{:#}", a(bad).unwrap_err());
        assert!(e.starts_with(why), "{bad:?}: {e}");
    }
}
/// `torq agent session start` (its id on standard output) and `torq agent session taint <id>
/// <source>`: the agent's session at the broker, which keeps its record (ADR-042, amended). The
/// agent's alone: the broker refuses anyone else's.
fn agent_session_command(args: &[String]) -> Result<i32> {
    let usage = "usage: torq agent session start | torq agent session taint <id> <source> | torq agent session show <id> (the agent's, run by torq-doctor)";
    let cli = Cli { repo: PathBuf::from(REPO), broker: if cfg!(debug_assertions) { std::env::var("TORQ_BROKER").unwrap_or_else(|_| BROKER.into()) } else { BROKER.into() } };
    let log = run::start_log()?;
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["start"] => {
            let v = done(run::broker_json(broker(&cli, &["session-start"])?, &log)?, "starting the agent's session")?;
            let id = agent_session(v["session"].as_str().map(String::from)).context("the broker named no session")?;
            println!("{id}");
        }
        ["taint", id, source] => {
            let id = agent_session(Some(id.to_string())).with_context(|| format!("{usage}: not a session's id"))?;
            let source = format!("--source={source}");
            done(run::broker_json(broker(&cli, &["session-taint", &id, &source])?, &log)?, "reporting what the session read")?;
        }
        ["show", id] => {
            let id = agent_session(Some(id.to_string())).with_context(|| format!("{usage}: not a session's id"))?;
            let v = done(run::broker_json(broker(&cli, &["session", &id])?, &log)?, "reading the session")?;
            println!("{v}");
        }
        _ => bail!("{usage}"),
    }
    Ok(0)
}

fn run_shell(args: &[String]) -> Result<i32> {
    // First-boot setup (the image's console, as root): its own program, not a shell.
    if args == ["setup"] {
        return crate::setup::run().map(|()| 0);
    }
    // The ISO installer (its console, as root, ADR-031): its own program too.
    if args.first().map(String::as_str) == Some("break-glass") {
        return crate::agent::break_glass(&args[1..]);
    }
    // The agent's session at the broker (ADR-042, amended): torq-doctor's way to it, as the agent.
    if args.first().map(String::as_str) == Some("agent") && args.get(1).map(String::as_str) == Some("session") {
        return agent_session_command(&args[2..]);
    }
    // The agent asked to propose (ADR-042, 6): the doctor, with one more tool.
    if args.first().map(String::as_str) == Some("agent") && args.get(1).map(String::as_str) == Some("propose") {
        return crate::agent::doctor_mode(&args[2..], true);
    }
    // A program run as the agent's account (ADR-041): its own entry, not a command of the shell.
    if args.first().map(String::as_str) == Some("agent") {
        return crate::agent::run(&args[1..]);
    }
    // Doctor mode (ADR-042): the agent asked one question, as the agent's account.
    if args.first().map(String::as_str) == Some("doctor") {
        return crate::agent::doctor_mode(&args[1..], false);
    }
    if args == ["install"] {
        return crate::install::run().map(|()| 0);
    }
    let name = own_name();
    let run = parse_args(&name, args)?;
    if run == Invocation::Help {
        say(&format!("{}\n\nWith no command: the shell (`help` there says how to use it). A command: that one operational\ncommand, its output on standard output. -c: commands separated by ;, stopping at the first that\nfails; their changes are their own (end with `commit` to keep them). --verbose: errors in full.\n", usage(&name)));
        return Ok(0);
    }
    // The overrides exist only in a development build (`cargo build`): the released torqcli (the Nix
    // package, a release build) always manages the machine's repo through its broker (second audit, L2).
    let env = |k: &str, d: &str| if cfg!(debug_assertions) { std::env::var(k).unwrap_or_else(|_| d.to_string()) } else { d.to_string() };
    let cli = Cli {
        repo: PathBuf::from(env("TORQ_CONFIG_REPO", REPO)),
        broker: env("TORQ_BROKER", BROKER),
    };
    check_repo(&cli)?;
    let schema = Schema::parse(&fs::read_to_string(SCHEMA).with_context(|| format!("reading {SCHEMA} (is this TorqOS?)"))?)?;
    let tree = crate::nixos::Tree::parse(&fs::read_to_string(NIXOS_SCHEMA).with_context(|| format!("reading {NIXOS_SCHEMA} (is this TorqOS, from S1 on?)"))?)?;
    let machine = Real { cli: &cli, detail: std::cell::Cell::new(false), asker: None };
    let who = me()?;
    // Only the interactive shell keeps a candidate between runs; a script's is its own.
    let saved = if matches!(run, Invocation::Interactive | Invocation::Draft(_)) { Some(Candidate::default_path()?) } else { None };
    let mut s = Session::new(schema, tree, Repo::new(&cli.repo), saved, &machine, &who);
    s.follow_schemas(Path::new(SCHEMA), Path::new(NIXOS_SCHEMA));
    s.screen = || crate::ui::columns(false);
    s.cache = crate::evalcache::Cache::personal();
    // Offered to go on (install what an update loaded) only where a person is at a terminal: never a
    // script, which must not stop on a question it was not written to answer.
    s.asks = asks(&run, std::io::stdin().is_terminal(), std::io::stdout().is_terminal());
    #[derive(Clone, Copy)]
    enum RunKind {
        Draft,
        Other,
    }
    let run_kind = if matches!(run, Invocation::Draft(_)) { RunKind::Draft } else { RunKind::Other };
    let fail = |line: &str, e: anyhow::Error| {
        say_error(&format!("{}: {}", history::one_line(line, 80), describe(&e)));
        Ok(1)
    };
    match run {
        Invocation::Script(script) | Invocation::Draft(script) => {
            let keeps = matches!(run_kind, RunKind::Draft);
            let lines = script_lines(&script);
            for (i, line) in lines.iter().enumerate() {
                match s.exec(line) {
                    Ok(out) => print_clean(&out),
                    Err(e) => {
                        let code = fail(line, e);
                        // Stopped there: say what did not run, so nothing is thought done that was not.
                        let rest = lines.len() - i - 1;
                        if rest > 0 {
                            eprint!("{}", crate::ui::Ui::for_terminal(true).line("  ", &format!("(stopped: the {} after it did not run)", if rest == 1 { "command".to_string() } else { format!("{rest} commands") })));
                            eprintln!();
                        }
                        return code;
                    }
                }
            }
            // A script's candidate is its own: what it changed and did not commit is gone. Say so.
            if !keeps && s.uncommitted().unwrap_or(false) {
                eprint!("{}", s.pending_note(crate::session::Pending::Discarded));
            }
            Ok(0)
        }
        Invocation::Command(w) => {
            // A `|` word is the pipe: shown as one, not quoted.
            let shown = w.iter().map(|x| if x == "|" { x.clone() } else { crate::syntax::quote(x) }).collect::<Vec<_>>().join(" ");
            match s.exec_words(&w) {
                Ok(out) => {
                    print_clean(&out);
                    Ok(0)
                }
                Err(e) => fail(&shown, e),
            }
        }
        Invocation::Interactive => {
            let history_cli = Cli { repo: cli.repo.clone(), broker: cli.broker.clone() };
            let history = crate::complete::Later::new(move || {
                let real = Real { cli: &history_cli, detail: std::cell::Cell::new(false), asker: None };
                real.configurations(&Repo::new(&history_cli.repo), 30).map(|h| crate::session::configuration_lines(&h)).unwrap_or_default()
            });
            interactive(&mut s, history)
        }
        Invocation::Help => unreachable!("answered before anything is read"),
    }
}

/// Output line by line, each cleaned of anything that could fool a terminal (the configuration is
/// checked on load, but the ledger's reasons and messages are not ours).
fn print_clean(out: &str) {
    say(&clean(out));
}

/// Output as it is printed: each line cleaned, then coloured (paint.rs: on what is already clean),
/// in the console's own characters (ui::for_console).
fn clean(out: &str) -> String {
    let cleaned: String = out.lines().map(|l| format!("{}\n", history::clean(l))).collect();
    crate::ui::for_console(&crate::paint::paint(&cleaned, &crate::ui::Ui::for_terminal(false)))
}

/// Output taller than the window, a screenful at a time, as on a network device: Space for the next
/// screenful, Enter for one line more, `n` for the rest at once, `q` (or Ctrl-C) to stop. All at
/// once when it fits, when standard output or input is not a terminal, and when more input is
/// already waiting (a paste: its next lines are not keys).
fn paged(text: &str) {
    let (Some(rows), Some(cols)) = (crate::ui::rows(), crate::ui::columns(false)) else { return say(text) };
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    // How many rows of the window a line takes (a line wider than it wraps).
    let height = |l: &str| crate::ui::width(l.trim_end_matches('\n')).div_ceil(cols).max(1);
    let total: usize = lines.iter().map(|l| height(l)).sum();
    if rows < 3 || total < rows || waiting() {
        return say(text);
    }
    let (mut next, mut room) = (0, rows - 1);
    while next < lines.len() {
        let mut used = 0;
        let start = next;
        while next < lines.len() && used + height(lines[next]) <= room.max(height(lines[next])) {
            used += height(lines[next]);
            next += 1;
        }
        say(&lines[start..next].concat());
        if next >= lines.len() {
            return;
        }
        let shown: usize = lines[..next].iter().map(|l| height(l)).sum();
        let more = format!("---(more {}%)---", shown * 100 / total);
        say(&more);
        // A key that means nothing here is not a reason to lose the rest: asked again (`h`: the keys).
        loop {
            let key = key();
            say("\r\x1b[K");
            match key {
                Some(b' ' | b'f') => room = rows - 1,
                Some(b'\r' | b'\n' | b'j') => room = 1,
                Some(b'n' | b'N') => return say(&lines[next..].concat()),
                // q, Ctrl-C (it reaches here as a key), the end of input: stop.
                Some(b'q' | b'Q' | 3) | None => return,
                Some(b'h' | b'H' | b'?') => {
                    say(&format!("Space: next page · Enter: one line · n: the rest · q: stop  {more}"));
                    continue;
                }
                Some(_) => {
                    say(&more);
                    continue;
                }
            }
            break;
        }
    }
}

/// The machine's network interfaces, by name (the kernel's list).
fn interfaces() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/class/net").map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

/// Whether more input is waiting to be read already (a paste).
fn waiting() -> bool {
    let stdin = std::io::stdin();
    let mut fds = [rustix::event::PollFd::new(&stdin, rustix::event::PollFlags::IN)];
    rustix::event::poll(&mut fds, Some(&rustix::event::Timespec { tv_sec: 0, tv_nsec: 0 })).is_ok_and(|n| n > 0)
}

/// One key from the terminal, as it is pressed (not echoed, Ctrl-C a key like any other); None if
/// there is no terminal to read.
fn key() -> Option<u8> {
    let stdin = std::io::stdin();
    let was = rustix::termios::tcgetattr(&stdin).ok()?;
    let mut raw = was.clone();
    raw.local_modes.remove(rustix::termios::LocalModes::ICANON | rustix::termios::LocalModes::ECHO | rustix::termios::LocalModes::ISIG);
    raw.special_codes[rustix::termios::SpecialCodeIndex::VMIN] = 1;
    raw.special_codes[rustix::termios::SpecialCodeIndex::VTIME] = 0;
    rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &raw).ok()?;
    let mut b = [0u8; 1];
    let got = rustix::io::read(&stdin, &mut b);
    let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &was);
    got.ok().filter(|&n| n == 1).map(|_| b[0])
}

/// One cause of an error as a person reads it: the system's own words for a failed file operation
/// ("No such file or directory"), without the number after them ("(os error 2)").
fn said(cause: &(dyn std::error::Error + 'static)) -> String {
    let text = cause.to_string();
    match cause.downcast_ref::<std::io::Error>().and_then(std::io::Error::raw_os_error) {
        Some(code) => text.strip_suffix(&format!(" (os error {code})")).map(str::to_string).unwrap_or(text),
        None => text,
    }
}

/// `error: …` to standard error, the word in red on a terminal.
fn say_error(msg: &str) {
    let ui = crate::ui::Ui::for_terminal(true);
    eprint!("{}", ui.error(msg));
}

/// Write to standard output. Whoever reads it stopped (`torqcli show configuration | head`): stop
/// too, quietly, as a tool killed by SIGPIPE would (141), not with a panic. Any other failure to
/// write (a full disk) is an error, never a quiet success with the output missing.
fn say(text: &str) {
    let mut o = std::io::stdout().lock();
    if let Err(e) = o.write_all(text.as_bytes()).and_then(|_| o.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(141);
        }
        eprintln!("error: writing the output: {e}");
        std::process::exit(1);
    }
}

/// The prompt, and a line above it: `[edit system]` then `user@host# ` in configuration mode;
/// `user@host> ` in operational mode.
fn prompt(s: &Session, user: &str, host: &str) -> (Option<String>, String) {
    let path: Vec<String> = s.edit_path().iter().map(|w| history::clean(w)).collect();
    match s.mode {
        Mode::Operational => (None, format!("{user}@{host}> ")),
        Mode::Configuration => {
            let at = if path.is_empty() { "[edit]".to_string() } else { format!("[edit {}]", path.iter().map(|w| crate::syntax::quote(w)).collect::<Vec<_>>().join(" ")) };
            (Some(format!("\n{at}")), format!("{user}@{host}# "))
        }
    }
}

/// The interactive shell: with a line editor (history, Tab, `?`) on a terminal, one plain line at a
/// time otherwise (piped input).
fn interactive(s: &mut Session, history: crate::complete::Later) -> Result<i32> {
    let host = fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    let user = me()?;
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return editor::run(s, &user, &host, history);
    }
    let stdin = std::io::stdin();
    loop {
        s.follow_schemas(Path::new(SCHEMA), Path::new(NIXOS_SCHEMA));
        let (above, p) = prompt(s, &user, &host);
        if let Some(a) = above {
            say(&format!("{a}\n"));
        }
        say(&p);
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            say("\n");
            return Ok(0);
        }
        // Piped lines are a script: stop at the first error, so a later `commit` cannot commit
        // what an earlier failed line did not change.
        if !run_line(s, line.trim(), false) {
            return Ok(1);
        }
        if s.done {
            return Ok(0);
        }
    }
}

/// Ask a person at the terminal; false when there is no one to ask. Only an answer typed after the
/// question counts: anything already waiting (typed or pasted ahead, e.g. a "y" after "commit") is
/// not run — a question ends a batch; the line editor names what it had already read
/// (editor::run), and what the terminal still holds is said here.
fn ask_person(question: &str) -> crate::session::Answer {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return crate::session::Answer::NobodyToAsk;
    }
    ASKED.store(true, std::sync::atomic::Ordering::Relaxed);
    let stdin = std::io::stdin();
    if rustix::io::ioctl_fionread(&stdin).is_ok_and(|n| n > 0) {
        let _ = rustix::termios::tcflush(&stdin, rustix::termios::QueueSelector::IFlush);
        println!("(the rest of what was typed or pasted ahead is not run: a question takes only an answer typed after it)");
    }
    print!("{}[y/N] ", crate::paint::paint(question, &crate::ui::Ui::for_terminal(false)));
    let _ = std::io::stdout().flush();
    let _seen = Echo::set(true);
    let mut answer = String::new();
    if stdin.lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "yes") {
        crate::session::Answer::Yes
    } else {
        crate::session::Answer::No
    }
}

/// The terminal's echo set on or off, and back as it was when this is dropped: off while a command
/// runs (keys typed meanwhile wait, unseen, for the next line instead of drawing over its output); on
/// while a person answers a question or pastes for `load … terminal` inside one.
pub(crate) struct Echo(Option<rustix::termios::Termios>);

impl Echo {
    pub(crate) fn set(on: bool) -> Self {
        let stdin = std::io::stdin();
        let was = rustix::termios::tcgetattr(&stdin).ok();
        if let Some(t) = &was {
            let mut now = t.clone();
            if on {
                now.local_modes.insert(rustix::termios::LocalModes::ECHO);
            } else {
                now.local_modes.remove(rustix::termios::LocalModes::ECHO);
            }
            let _ = rustix::termios::tcsetattr(&stdin, rustix::termios::OptionalActions::Now, &now);
        }
        Echo(was)
    }
}

impl Drop for Echo {
    fn drop(&mut self) {
        if let Some(t) = &self.0 {
            let _ = rustix::termios::tcsetattr(std::io::stdin(), rustix::termios::OptionalActions::Now, t);
        }
    }
}

/// Set when a question was asked (`Real::ask`): it ends the batch the line was part of.
static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Run one line; false if it failed (the error is printed with the line: in a paste, the first line
/// cannot know more follow, so every error names its line). `typed`: a person typed it, and waits
/// for it — its output may be shown a screenful at a time (not a script's, nor a paste's).
fn run_line(s: &mut Session, line: &str, typed: bool) -> bool {
    match s.exec(line) {
        Ok(out) if s.page && typed => {
            paged(&clean(&out));
            true
        }
        Ok(out) => {
            print_clean(&out);
            true
        }
        Err(e) => {
            print!("{}", crate::ui::Ui::for_terminal(false).error(&format!("{}: {}", history::one_line(line, 80), describe(&e))));
            false
        }
    }
}

/// The line editor: rustyline, with Tab and `?` answered by complete.rs.
mod editor {
    use super::{ask_person, prompt, run_line, say_error, Echo, ASKED};
    use std::sync::atomic::Ordering;
    use crate::complete;
    use crate::session::{words, Mode, Session};
    use anyhow::Result;
    use rustyline::completion::{Completer, Pair};
    use rustyline::error::ReadlineError;
    use rustyline::highlight::Highlighter;
    use rustyline::hint::Hinter;
    use rustyline::validate::Validator;
    use rustyline::{Cmd, ConditionalEventHandler, Context, Event, EventContext, EventHandler, KeyEvent, RepeatCount};
    use std::sync::{Arc, Mutex};

    /// What Tab and `?` need: the schema, and the mode the session is in now.
    #[derive(Clone)]
    struct Help {
        /// What `?` and Tab know: the schema and the NixOS tree (complete::Know borrows them).
        know: Arc<Mutex<Arc<(crate::tree::Schema, crate::nixos::Tree)>>>,
        /// Where the session is: mode, edit path (updated before each line is read).
        at: Arc<Mutex<complete::At>>,
    }

    /// The complete words before the cursor, and the one being typed. None inside a quoted string.
    fn at_cursor(before: &str) -> Option<(Vec<String>, String)> {
        // Inside an open quote (or a line that does not split): `?` is a character.
        if words(before).is_err() {
            return None;
        }
        let partial = if before.ends_with(char::is_whitespace) { "" } else { before.rsplit(char::is_whitespace).next().unwrap_or("") };
        let done = &before[..before.len() - partial.len()];
        Some((words(done).ok()?, partial.to_string()))
    }

    impl Help {
        fn choices(&self, before: &str) -> Option<(Vec<complete::Choice>, String)> {
            let (w, partial) = at_cursor(before)?;
            let at = self.at.lock().unwrap().clone();
            let knows = self.know.lock().unwrap().clone();
            let know = complete::Know { schema: &knows.0, tree: &knows.1 };
            Some((complete::choices_at(&know, &at, &w, &partial), partial))
        }
    }

    impl Completer for Help {
        type Candidate = Pair;
        fn complete(&self, line: &str, pos: usize, _: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
            let Some((choices, partial)) = self.choices(&line[..pos]) else { return Ok((pos, vec![])) };
            let pairs = complete::tab(&choices).into_iter().map(|(display, replacement)| Pair { display, replacement }).collect();
            Ok((pos - partial.len(), pairs))
        }
    }
    impl Hinter for Help {
        type Hint = String;
    }
    impl Highlighter for Help {
        // The prompt in colour (paint.rs); rustyline measures it from the plain text.
        fn highlight_prompt<'b, 's: 'b, 'p: 'b>(&'s self, prompt: &'p str, _default: bool) -> std::borrow::Cow<'b, str> {
            std::borrow::Cow::Owned(crate::paint::prompt(prompt, &crate::ui::Ui::for_terminal(false)))
        }
    }
    impl Validator for Help {}
    impl rustyline::Helper for Help {}

    /// A line that was read sooner than this after the prompt was waiting already: no person types a
    /// whole command in a quarter of a second.
    const WAITING: std::time::Duration = std::time::Duration::from_millis(250);

    /// `?`: list what may come next, and redraw the line as it was.
    struct Question(Help);
    impl ConditionalEventHandler for Question {
        fn handle(&self, _: &Event, _: RepeatCount, _: bool, ctx: &EventContext) -> Option<Cmd> {
            let (w, partial) = at_cursor(&ctx.line()[..ctx.pos()])?; // in quotes: a plain '?'
            let at = self.0.at.lock().unwrap().clone();
            let knows = self.0.know.lock().unwrap().clone();
            let know = complete::Know { schema: &knows.0, tree: &knows.1 };
            let answer = complete::answer(&know, &at, &w, &partial, crate::ui::Ui::for_terminal(false).width());
            let text: String = answer.lines().map(|l| format!("{}\n", crate::history::clean(l))).collect();
            let text = crate::paint::paint(&text, &crate::ui::Ui::for_terminal(false));
            print!("?\r\n{}", text.replace('\n', "\r\n"));
            Some(Cmd::Repaint)
        }
    }


    #[cfg(test)]
    #[test]
    fn the_word_at_the_cursor() {
        let v = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(at_cursor("set sys"), Some((v("set"), "sys".into())));
        assert_eq!(at_cursor("set system "), Some((v("set system"), "".into())));
        assert_eq!(at_cursor(""), Some((vec![], "".into())));
        assert_eq!(at_cursor("commit comment \"why"), None, "inside quotes: `?` is a character");
        assert_eq!(at_cursor("commit comment \"why\" "), Some((vec!["commit".into(), "comment".into(), "why".into()], "".into())));
        assert_eq!(at_cursor("show | grep 'a b"), None, "inside single quotes too");
    }

    pub fn run(s: &mut Session, user: &str, host: &str, history: complete::Later) -> Result<i32> {
        // (Ctrl-C while a command runs: the line editor keeps SIGINT for as long as it lives, so it
        // stops the command's child — nix, the broker — and torqcli says so and stays, its terminal
        // put back by the Echo guard as the command returns.)
        let at = |s: &mut Session| complete::At { mode: s.mode, edit: s.edit_path().to_vec(), data: s.help_data(), configurations: history.clone(), files: s.nix_files(), interfaces: super::interfaces() };
        let help = Help { know: Arc::new(Mutex::new(Arc::new((s.schema.clone(), s.tree.clone())))), at: Arc::new(Mutex::new(at(s))) };
        // Bracketed paste off: a paste arrives as typed, each line running at its newline as on a network
        // device (not held as one block until another Enter); the lines after the first were waiting
        // already, so they are one batch (below), and none is lost (rustyline's buffer-redux).
        let config = rustyline::Config::builder()
            .completion_type(rustyline::CompletionType::List)
            // (Added by hand: a line of only spaces is not one to come back to.)
            .auto_add_history(false)
            .bracketed_paste(false)
            .build();
        let mut ed: rustyline::Editor<Help, rustyline::history::DefaultHistory> = rustyline::Editor::with_config(config)?;
        ed.set_helper(Some(help.clone()));
        ed.bind_sequence(KeyEvent::from('?'), EventHandler::Conditional(Box::new(Question(help.clone()))));
        // Lines that failed since the person last looked at `show | compare` (or committed); whether
        // a question (or `load … terminal`) ended the batch being read.
        let (mut failed, mut after_question) = (0usize, false);
        loop {
            // Updated since the last line: `?` and Tab know the new system's settings.
            if s.follow_schemas(std::path::Path::new(super::SCHEMA), std::path::Path::new(super::NIXOS_SCHEMA)) {
                *help.know.lock().unwrap() = Arc::new((s.schema.clone(), s.tree.clone()));
            }
            *help.at.lock().unwrap() = at(s);
            let (above, p) = prompt(s, user, host);
            if let Some(a) = above {
                println!("{}", crate::paint::paint(&a, &crate::ui::Ui::for_terminal(false)));
            }
            let asked = std::time::Instant::now();
            match ed.readline(&p) {
                // As on a network device, every line of a paste runs, and each error names its line.
                // A `commit` while lines have failed since the person last looked at `show | compare`
                // asks first — a question a paste cannot answer, so a commit never takes part of a
                // paste unseen. (No timing decides that; timing only tells which lines came before
                // a question: those still waiting when it was asked.)
                Ok(text) => {
                    if !text.trim().is_empty() {
                        let _ = ed.add_history_entry(text.as_str());
                    }
                    // Typed, not pasted: the prompt waited for it (a paste's later lines come at once).
                    let typed = asked.elapsed() >= WAITING;
                    if typed {
                        after_question = false;
                    }
                    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
                    let mut lines = lines.into_iter();
                    while let Some(line) = lines.next() {
                        // A question ended this batch: what came after it was pasted, not answered.
                        if after_question {
                            println!("(not run, pasted before the shell asked for input: {})", crate::history::one_line(&crate::history::clean(line), 80));
                            continue;
                        }
                        let commits = s.commits(line);
                        if failed > 0 && commits {
                            let lines = if failed == 1 { "1 line".to_string() } else { format!("{failed} lines") };
                            let anyway = ask_person(&format!("{lines} failed since you last looked at `show | compare`. Commit anyway? ")) == crate::session::Answer::Yes;
                            ASKED.store(false, Ordering::Relaxed);
                            after_question = true;
                            if !anyway {
                                say_error(&format!("{line}: not committed"));
                                continue;
                            }
                        }
                        // (Typing goes on while a command runs: keys wait, unseen, for the next line.)
                        let ok = {
                            let _quiet = Echo::set(false);
                            run_line(s, line, typed && lines.len() == 0)
                        };
                        if !ok {
                            failed += 1;
                        } else if commits || s.reviews(line) {
                            failed = 0;
                        }
                        after_question |= ASKED.swap(false, Ordering::Relaxed);
                        if s.done {
                            break;
                        }
                    }
                }
                // Ctrl-C: drop the line, as a shell does.
                Err(ReadlineError::Interrupted) => continue,
                // Ctrl-D: as `exit` from wherever it is, out of configuration mode (saying what is
                // kept) and out of torqcli.
                // Ctrl-D: out of configuration mode (as on a network device), or, from operational
                // mode, out of torqcli — each as its `exit` is, saying what is kept.
                Err(ReadlineError::Eof) => {
                    println!();
                    if s.mode == Mode::Configuration {
                        run_line(s, "exit configuration-mode", false);
                        continue;
                    }
                    run_line(s, "exit", false);
                    return Ok(0);
                }
                Err(e) => return Err(e.into()),
            }
            if s.done {
                return Ok(0);
            }
        }
    }
}

/// Whether a change touches only `me`'s own folder (user tier, every file under `users/<me>/`): the
/// one kind a revert or a commit makes without asking.
fn only_own_folder(tier: &str, files: &[String], me: &str) -> bool {
    let own = format!("users/{me}/");
    tier == "user" && !files.is_empty() && files.iter().all(|f| f.starts_with(&own) && !f[own.len()..].contains(".."))
}

#[cfg(test)]
#[test]
fn only_ones_own_folder_goes_without_asking() {
    let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert!(only_own_folder("user", &f(&["users/viper/packages.json"]), "viper"));
    assert!(!only_own_folder("user", &f(&["users/viper/packages.json", "users/eve/packages.json"]), "viper"), "someone else's");
    assert!(!only_own_folder("user", &f(&["users/viperx/packages.json"]), "viper"), "a longer name is not yours");
    assert!(!only_own_folder("user", &f(&["users/viper/../eve/packages.json"]), "viper"));
    assert!(!only_own_folder("system", &f(&["users/viper/packages.json"]), "viper"), "the system tier always asks");
    assert!(!only_own_folder("user", &[], "viper"), "no files: nothing known, ask");
}

/// Where the ledger says the config repo is: the commit of its last entry that moved it — the
/// broker's own count (`ledgered_commit`, broker/src/lib.rs), repairs and interrupted changes too.
fn ledgered_commit(entries: &[history::Entry]) -> Option<String> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.kind.as_str(), "init" | "adopted" | "applied" | "reverted" | "interrupted" | "repaired" | "rolled_back"))
        .find_map(|e| e.data["commit"].as_str().map(String::from))
}

/// The configurations in a ledger, newest first: its `init`, `applied`, `reverted` and `rolled_back`
/// entries (each made a commit), at most `count`. Text from the ledger is one line each, cleaned.
fn configurations_of(entries: &[history::Entry], count: usize, clock: &history::Clock) -> Vec<Configuration> {
    entries
        .iter()
        .rev()
        .filter(|e| matches!(e.kind.as_str(), "init" | "adopted" | "applied" | "reverted" | "rolled_back"))
        .filter_map(|e| {
            let id = e.data["commit"].as_str()?;
            let who = e.approved_by.clone().filter(|w| !w.is_empty()).unwrap_or_else(|| e.actor.clone());
            Some(Configuration {
                id: id.to_string(),
                // Local time, as the ledger shows it (`show system ledger`).
                date: history::local_stamp(&e.ts, clock),
                who: history::one_line(&who, 20),
                // Whole: the table wraps it in a window (ui::Table).
                what: history::one_line(&e.reason, usize::MAX),
                seq: Some(e.seq),
            })
        })
        .take(count)
        .collect()
}

#[cfg(test)]
#[test]
fn configurations_are_the_ledger_entries_that_made_one() {
    let ledger = [
        r#"{"seq":1,"ts":"2026-10-31T01:00:00Z","actor":"user","approved_by":null,"kind":"init","tier":"system","reason":"Config repository initialised","data":{"commit":"aaaa"}}"#,
        r#"{"seq":2,"ts":"2026-10-31T02:00:00Z","actor":"user","approved_by":null,"kind":"proposal","tier":"user","reason":"x","data":{}}"#,
        r#"{"seq":3,"ts":"2026-10-31T02:01:00Z","actor":"user","approved_by":"viper","kind":"applied","tier":"user","reason":"+   style c;","data":{"commit":"bbbb","proposal":2}}"#,
        r#"{"seq":4,"ts":"2026-10-31T02:02:00Z","actor":"user","approved_by":"viper","kind":"confirmed","tier":"user","reason":"Confirmed","data":{"applied":3}}"#,
        r#"{"seq":5,"ts":"2026-10-31T03:00:00Z","actor":"broker","approved_by":null,"kind":"rolled_back","tier":"user","reason":"Change #3 rolled back\nfake: line","data":{"commit":"aaaa","applied":3}}"#,
        r#"{"seq":6,"ts":"2026-10-31T04:00:00Z","actor":"user","approved_by":"viper","kind":"apply_failed","tier":"user","reason":"broke","data":{}}"#,
        // A repair returns to a configuration the ledger has: it makes none of its own.
        r#"{"seq":7,"ts":"2026-10-31T05:00:00Z","actor":"user","approved_by":"viper","kind":"repaired","tier":"system","reason":"Repaired","data":{"commit":"aaaa"}}"#,
    ]
    .join("\n");
    let entries = history::parse(&ledger).unwrap();
    // (An hour east of UTC: each date is the ledger's time, shown local.)
    let clock = history::Clock { now: 0, offset: 3600 };
    let c = configurations_of(&entries, 10, &clock);
    let ids: Vec<&str> = c.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["aaaa", "bbbb", "aaaa"], "newest first; a rollback is a configuration of its own");
    assert_eq!((c[0].who.as_str(), c[0].seq), ("broker", Some(5)));
    assert_eq!(c[1].who, "viper", "the person who approved it");
    assert_eq!(c[0].what, "Change #3 rolled back fake: line", "one line");
    assert_eq!(c[2].date, "2026-10-31 02:00:00", "the ledger's 01:00 UTC, local");
    assert_eq!(configurations_of(&entries, 1, &clock).len(), 1);
    assert_eq!(ledgered_commit(&entries).as_deref(), Some("aaaa"), "the repair is the newest entry that moved it");
    let more = format!("{ledger}\n{}", r#"{"seq":8,"ts":"t","actor":"broker","approved_by":null,"kind":"interrupted","tier":"user","reason":"i","data":{"commit":"cccc"}}"#);
    assert_eq!(ledgered_commit(&history::parse(&more).unwrap()).as_deref(), Some("cccc"), "as the broker counts: interrupted too");
}

/// Whether `prog` is a program on PATH.
fn which(prog: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

impl Real<'_> {
    /// The flake.lock `nix flake update` (of `torq`, or with `all` every input) makes from the
    /// configuration at `base`, its flake.nix replaced by `flake_nix` if given.
    fn relock(&self, base: &str, flake_nix: Option<&str>, all: bool) -> Result<String> {
        let scratch = scratch_dir()?;
        let copy = scratch.path().join("config");
        fs::create_dir(&copy)?;
        Repo::new(&self.cli.repo).archive(base, &copy)?;
        if let Some(f) = flake_nix {
            fs::write(copy.join("flake.nix"), f)?;
        }
        let out = {
            let _p = self.progress("Looking for updates");
            let mut cmd = std::process::Command::new("nix");
            cmd.args(["flake", "update", "--flake"]).arg(crate::flake_ref(&copy));
            if !all {
                cmd.arg("torq");
            }
            run::output(&mut cmd, &run::start_log()?, self.detail.get()).context("running nix flake update")?
        };
        if !out.status.success() {
            bail!("couldn't look for updates: {}", history::one_line(&crate::last_error(&out.stderr), 200));
        }
        fs::read_to_string(copy.join("flake.lock")).context("reading the updated flake.lock")
    }
}
