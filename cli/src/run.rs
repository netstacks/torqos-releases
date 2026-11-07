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
                if ASKING.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
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

/// Where the broker is (ADR-040): its socket, which needs no sudo — who is asking is the kernel's
/// answer — or a program run with the arguments (root runs the broker itself; a test broker).
pub enum Call {
    Socket(Vec<String>),
    Exec(Command),
}

/// The broker's socket (modules/broker.nix).
pub const SOCKET: &str = "/run/torq/broker.sock";
/// The exit code that says the broker needs the caller's password (torq_broker::socket).
const NEED_PASSWORD: i32 = 77;

/// What goes over the socket (torq_broker::socket::Request): the arguments without who is asking,
/// which the broker takes from the kernel, and files' contents rather than their paths.
#[derive(serde::Serialize, Default)]
struct Request {
    args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
}

/// What comes back (torq_broker::socket::Frame).
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Frame {
    Out(String),
    Err(String),
    Exit(i32),
}

/// The arguments torqcli builds for any broker, as a socket request: `--actor` and `--approved-by`
/// left out (the broker decides them), `--diff` and `--hash-file` sent as what they hold.
fn request(args: &[String]) -> Result<Request> {
    let mut r = Request::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).with_context(|| format!("{flag} needs a value"));
        match flag {
            "--actor" | "--approved-by" => {
                value()?;
            }
            "--diff" => r.diff = Some(fs::read_to_string(value()?).context("reading the change")?),
            "--hash-file" => r.hash = Some(fs::read_to_string(value()?).context("reading the hash")?),
            _ => r.args.push(a.clone()),
        }
    }
    Ok(r)
}

/// One request on the socket: its exit code (None: the broker hung up before saying) and its
/// standard output; its standard error to `err` as it comes.
fn over_socket(r: &Request, err: &mut dyn Write) -> Result<(Option<i32>, Vec<u8>)> {
    use std::io::BufRead;
    let mut s = std::os::unix::net::UnixStream::connect(SOCKET)
        .with_context(|| format!("reaching the broker at {SOCKET} (is torq-broker.socket running?)"))?;
    writeln!(s, "{}", serde_json::to_string(r)?)?;
    let (mut out, mut code) = (Vec::new(), None);
    for line in std::io::BufReader::new(s).lines() {
        match serde_json::from_str::<Frame>(&line.context("reading the broker's answer")?) {
            Ok(Frame::Out(t)) => out.extend_from_slice(t.as_bytes()),
            Ok(Frame::Err(t)) => {
                let _ = err.write_all(t.as_bytes());
            }
            Ok(Frame::Exit(c)) => code = Some(c),
            // A line cut short is the broker gone mid-write (its caller too slow, or serve ended):
            // what came is all there is, and no exit code — as if it hung up.
            Err(_) => break,
        }
    }
    Ok((code, out))
}

/// While a password is asked, the progress line keeps quiet.
static ASKING: AtomicBool = AtomicBool::new(false);

/// The caller's password, read from the terminal with echo off. torqcli is made non-dumpable
/// first: no other program of the same account can read it out of this one's memory (ADR-040).
fn ask_password(user: &str, again: bool) -> Result<String> {
    use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions};
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable).context("protecting the password")?;
    let mut tty = fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("there is no terminal to ask for the password at")?;
    ASKING.store(true, Ordering::Relaxed);
    let asked = (|| -> Result<String> {
        let old = tcgetattr(&tty).context("reading the terminal")?;
        let mut quiet = old.clone();
        quiet.local_modes.remove(LocalModes::ECHO);
        write!(tty, "\r\x1b[2K{}Password for {user}: ", if again { "That is not the password. " } else { "" })?;
        tty.flush()?;
        tcsetattr(&tty, OptionalActions::Now, &quiet).context("turning echo off")?;
        let mut line = String::new();
        let read = std::io::BufRead::read_line(&mut std::io::BufReader::new(&tty), &mut line);
        let restored = tcsetattr(&tty, OptionalActions::Now, &old);
        writeln!(tty)?;
        read.context("reading the password")?;
        restored.context("turning echo back on")?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    })();
    ASKING.store(false, Ordering::Relaxed);
    asked
}

/// Run the broker with `--json` (added once, if the arguments have not got it): its exit code and
/// standard output. Its standard error — the rebuild's output — goes to `err`. Over the socket, a
/// change that needs the caller's password asks for it at the terminal (three tries), and only then.
fn exec(call: Call, err: &mut dyn Write, err_stdio: impl Fn() -> Result<Stdio>) -> Result<(Option<i32>, Vec<u8>)> {
    match call {
        Call::Exec(mut c) => {
            if !c.get_args().any(|a| a == "--json") {
                c.arg("--json");
            }
            let out = c
                .stdin(Stdio::inherit()) // sudo may need to ask for a password
                .stdout(Stdio::piped())
                .stderr(err_stdio()?)
                .output()
                .context("running the broker")?;
            Ok((out.status.code(), out.stdout))
        }
        Call::Socket(args) => {
            let mut r = request(&args)?;
            if !r.args.iter().any(|a| a == "--json") {
                r.args.push("--json".into());
            }
            let mut answer = over_socket(&r, err)?;
            for again in [false, true, true] {
                if answer.0 != Some(NEED_PASSWORD) {
                    break;
                }
                if !std::io::stdin().is_terminal() {
                    break; // the broker's own words say why: it needs a password, and none can be asked
                }
                r.password = Some(ask_password(&crate::me()?, again)?);
                answer = over_socket(&r, err)?;
            }
            Ok(answer)
        }
    }
}

/// Run the broker with `--json`: its exit code and parsed stdout. stderr (and the rebuild's
/// output) goes to `log`; so does stdout when it is not a success, since with --json the broker's
/// own explanation is there and the log is where a person looks.
fn call(broker: Call, log: &std::path::Path) -> Result<(Option<i32>, Option<Value>)> {
    let mut log_file = fs::OpenOptions::new().append(true).create(true).open(log).context("opening the log")?;
    let for_child = || log_file.try_clone().map(Stdio::from).context("opening the log");
    let (code, stdout) = exec(broker, &mut log_file.try_clone().context("opening the log")?, for_child)?;
    if code != Some(0) {
        writeln!(log_file, "\ntorq-broker {code:?}: {}", String::from_utf8_lossy(&stdout).trim())?;
    }
    Ok((code, serde_json::from_slice(&stdout).ok()))
}

/// Run the broker for something it only reads (the ledger), its errors on the terminal: exit code
/// and standard output.
pub fn read(broker: Call) -> Result<(Option<i32>, Vec<u8>)> {
    exec(broker, &mut std::io::stderr(), || Ok(Stdio::inherit()))
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
pub fn broker_json(broker: Call, log: &std::path::Path) -> Result<Outcome> {
    let (code, v) = call(broker, log)?;
    Ok(match (code, v) {
        (Some(0), Some(v)) => Outcome::Done(v),
        (Some(75), _) => Outcome::Busy,
        // 77: it needs the caller's password, and none was given or it was wrong (ADR-040).
        (Some(1 | 77), Some(v)) => Outcome::Refused(v["error"].as_str().unwrap_or_default().to_string()),
        (code, _) => Outcome::Broken(broken(code)),
    })
}

/// `status --json` is an answer whatever its exit code — 0 all well, 1 problems found, 75 busy —
/// so its JSON is returned for all three (or why there is none).
pub fn broker_status(broker: Call, log: &std::path::Path) -> Result<std::result::Result<Value, String>> {
    let (code, v) = call(broker, log)?;
    Ok(match (code, v) {
        (Some(0 | 1 | 75), Some(v)) if v.get("error").is_none() => Ok(v),
        (Some(1), Some(v)) => Err(crate::history::one_line(v["error"].as_str().unwrap_or("the broker refused"), 200)),
        (code, _) => Err(broken(code)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// ADR-040: over the socket, who is asking is never sent (the broker takes it from the kernel),
    /// and a file goes as what it holds, not a path for root to open.
    #[test]
    fn a_request_names_no_one_and_no_file() {
        let d = tempfile::tempdir().unwrap();
        let diff = d.path().join("change.diff");
        fs::write(&diff, "--- a/x\n+++ b/x\n").unwrap();
        let r = request(&args(&["propose", &format!("--diff={}", diff.display()), "--base=abc", "--reason=-  host-name two", "--actor=user"])).unwrap();
        assert_eq!(r.args, args(&["propose", "--base=abc", "--reason=-  host-name two"]));
        assert_eq!(r.diff.as_deref(), Some("--- a/x\n+++ b/x\n"));
        let r = request(&args(&["apply", "7", "--approved-by", "viper", "--confirm", "5"])).unwrap();
        assert_eq!(r.args, args(&["apply", "7", "--confirm", "5"]));
        let hash = d.path().join("hash");
        fs::write(&hash, "$6$salt$abc\n").unwrap();
        let r = request(&args(&["password", "--user", "sam", "--hash-file", &hash.to_string_lossy(), "--approved-by=viper"])).unwrap();
        assert_eq!(r.args, args(&["password", "--user", "sam"]));
        assert_eq!(r.hash.as_deref(), Some("$6$salt$abc\n"));
        assert!(request(&args(&["revert", "3", "--approved-by"])).is_err(), "a flag without its value");
        let line = serde_json::to_string(&request(&args(&["status"])).unwrap()).unwrap();
        assert_eq!(line, r#"{"args":["status"]}"#, "no password, diff or hash unless there is one");
    }
}
