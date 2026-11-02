//! Running the broker quietly (docs/CLI-DESIGN.md, ADR-014): its output goes to a log file, the
//! terminal shows one progress line, and its result is read as JSON — never parsed from its text.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// `$XDG_STATE_HOME/torq/last.log` (default `~/.local/state/torq/last.log`): the full output
/// of the last change, shown only when something fails.
/// Never a shared directory: without a home there is no log (it would be readable, and replaceable,
/// by others in /tmp).
pub fn log_path() -> Result<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .context("neither XDG_STATE_HOME nor HOME is set, so there is nowhere private to keep the log")?;
    Ok(state.join("torq/last.log"))
}

/// Start a fresh log for this command: private to the user (0600), and never written through a
/// symlink.
pub fn start_log() -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = log_path()?;
    fs::create_dir_all(p.parent().unwrap()).with_context(|| format!("creating {}", p.display()))?;
    if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(&p).with_context(|| format!("removing the symlink {}", p.display()))?;
    }
    let f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&p)
        .with_context(|| format!("writing {}", p.display()))?;
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(fs::Permissions::from_mode(0o600))?; // also for a log created before this
    Ok(p)
}

/// One line on the terminal while something runs: a spinner, the phase and the time so far.
/// Nothing at all when stderr is not a terminal. Cleared when dropped.
pub struct Progress {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Progress {
    pub fn start(phase: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        // A dumb terminal cannot redraw a line in place: show nothing rather than a stream of frames.
        if !std::io::stderr().is_terminal() || std::env::var("TERM").is_ok_and(|t| t == "dumb") {
            return Progress { stop, handle: None };
        }
        let (flag, phase) = (stop.clone(), phase.to_string());
        let handle = std::thread::spawn(move || {
            // (The machine's own console has no braille in its font: a plain spinner there.)
            let frames: &[char] = if crate::ui::on_console() { &['|', '/', '-', '\\'] } else { &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] };
            let started = Instant::now();
            let mut i = 0;
            while !flag.load(Ordering::Relaxed) {
                let line = crate::ui::for_console(&format!("  {} {phase}…  {}", frames[i % frames.len()], crate::ui::elapsed(started.elapsed())));
                eprint!("\r\x1b[2K{line}");
                let _ = std::io::stderr().flush();
                std::thread::sleep(Duration::from_millis(100));
                i += 1;
            }
            eprint!("\r\x1b[2K");
            let _ = std::io::stderr().flush();
        });
        Progress { stop, handle: Some(handle) }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// How a broker command ended, read from its exit code and JSON (CLI-DESIGN.md, "The broker's
/// JSON interface"). Messages are the broker's text and must be cleaned before printing.
#[derive(Debug)]
pub enum Outcome {
    Done(Value),
    /// Another operation is running (exit 75).
    Busy,
    /// Refused or failed (exit 1), with the broker's message.
    Refused(String),
    /// The broker itself did not answer as it should (crash, bad arguments, not JSON).
    Broken(String),
}

/// Run `broker` (already built with its arguments) with `--json`: its exit code and parsed stdout.
/// stderr (and the rebuild's output) goes to `log`; so does stdout when it is not a success, since
/// with --json the broker's own explanation is there and the log is where a person looks.
fn call(mut broker: Command, log: &std::path::Path) -> Result<(Option<i32>, Option<Value>)> {
    let log_file = fs::OpenOptions::new().append(true).create(true).open(log).context("opening the log")?;
    let out = broker
        .arg("--json")
        .stdin(Stdio::inherit()) // sudo may need to ask for a password
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log_file))
        .output()
        .context("running the broker")?;
    if out.status.code() != Some(0) {
        let mut f = fs::OpenOptions::new().append(true).open(log).context("opening the log")?;
        writeln!(f, "\ntorq-broker {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stdout).trim())?;
    }
    Ok((out.status.code(), serde_json::from_slice(&out.stdout).ok()))
}

fn broken(code: Option<i32>) -> String {
    match code {
        // 2: it did not understand the request — an installed broker older than this command.
        Some(2) => "the installed broker is older than this torq command (update the machine first)".into(),
        Some(code) => format!("the broker stopped unexpectedly (exit code {code})"),
        None => "the broker was stopped before it finished".into(),
    }
}

/// A broker command that changes something, or reads one thing: done, busy, refused, or broken.
pub fn broker_json(broker: Command, log: &std::path::Path) -> Result<Outcome> {
    let (code, v) = call(broker, log)?;
    Ok(match (code, v) {
        (Some(0), Some(v)) => Outcome::Done(v),
        (Some(75), _) => Outcome::Busy,
        (Some(1), Some(v)) => Outcome::Refused(v["error"].as_str().unwrap_or_default().to_string()),
        (code, _) => Outcome::Broken(broken(code)),
    })
}

/// `status --json` is an answer whatever its exit code — 0 all well, 1 problems found, 75 busy —
/// so its JSON is returned for all three (or why there is none).
pub fn broker_status(broker: Command, log: &std::path::Path) -> Result<std::result::Result<Value, String>> {
    let (code, v) = call(broker, log)?;
    Ok(match (code, v) {
        (Some(0 | 1 | 75), Some(v)) if v.get("error").is_none() => Ok(v),
        (Some(1), Some(v)) => Err(crate::history::one_line(v["error"].as_str().unwrap_or("the broker refused"), 200)),
        (code, _) => Err(broken(code)),
    })
}
