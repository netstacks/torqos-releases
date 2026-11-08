//! `torq-broker serve`: one request on the socket (ADR-040, `torq_broker::socket`).
//!
//! Its standard input and output are the connection (systemd, `Accept=yes`). It learns who is at
//! the other end from the kernel, reads the request, decides what that account may do, checks a
//! password where one is needed, and then runs the broker — this same program, as root — with the
//! request's arguments and the identity it decided. What the broker prints comes back as frames;
//! a caller that hangs up stops nothing: a change already started finishes and is ledgered.

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};
use torq_broker::config::{self, Settings};
use torq_broker::ledger::Ledger;
use torq_broker::policy::Tier;
use torq_broker::socket::{self, Caller, Change, Frame, Gate, Request};

/// How many frames may wait for the caller. Past that, lines are left out — counted, and said —
/// rather than the broker's output ever waiting on the caller (the reviews of ADR-040).
const QUEUE: usize = 4096;
/// How much of the broker's standard output (its answer) is kept for a caller that has fallen behind.
const KEEP_BYTES: usize = 16 << 20;
/// Where a request's files are kept while it is answered: a tmpfs, root's (modules/broker.nix).
const SCRATCH: &str = "/run/torq";

/// The connection's output: a frame per line, written by a thread of its own from a bounded queue.
/// Sending never waits on the caller: when the caller falls behind, lines are left out and the
/// caller is told how many; a caller that has gone (its connection broken, or not read for the
/// send timeout) gets nothing more. Nothing the broker does stops because of either.
#[derive(Clone)]
struct Reply {
    queue: SyncSender<Frame>,
    gone: Arc<AtomicBool>,
    skipped: Arc<AtomicU64>,
}

impl Reply {
    /// The reply, and its writer's end (to wait for, a while, at the end).
    fn start() -> (Reply, Receiver<()>) {
        let (queue, frames) = sync_channel::<Frame>(QUEUE);
        let (gone, skipped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU64::new(0)));
        let (done, finished) = sync_channel(1);
        let (g, s) = (gone.clone(), skipped.clone());
        std::thread::spawn(move || {
            let mut out = std::io::BufWriter::with_capacity(256 * 1024, std::io::stdout().lock());
            while let Ok(first) = frames.recv() {
                // Everything waiting, in one go; then what was left out, said; then flushed.
                put(&mut out, &g, &first);
                while let Ok(more) = frames.try_recv() {
                    put(&mut out, &g, &more);
                }
                let n = s.swap(0, Ordering::Relaxed);
                if n > 0 {
                    put(&mut out, &g, &Frame::Err(format!("[{n} lines of output not shown: they came faster than they could be sent]\n")));
                }
                if !g.load(Ordering::Relaxed) && out.flush().is_err() {
                    g.store(true, Ordering::Relaxed);
                }
            }
            let _ = done.send(());
        });
        (Reply { queue, gone, skipped }, finished)
    }

    /// While the broker runs: never waits. A full queue leaves the frame out, and counts it.
    fn send(&self, f: Frame) {
        if self.gone.load(Ordering::Relaxed) {
            return;
        }
        match self.queue.try_send(f) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.skipped.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(_)) => self.gone.store(true, Ordering::Relaxed),
        }
    }

    /// Standard output's way in: queued, or false when the queue is full (the frame then waits).
    fn offer(&self, f: &Frame) -> bool {
        if self.gone.load(Ordering::Relaxed) {
            return true;
        }
        match self.queue.try_send(f.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => false,
            Err(TrySendError::Disconnected(_)) => {
                self.gone.store(true, Ordering::Relaxed);
                true
            }
        }
    }

    /// Once the broker is done (its lock released, so waiting harms no one): the frame is sent,
    /// waiting for room, until `deadline`.
    fn send_last(&self, mut f: Frame, deadline: Instant) {
        while !self.gone.load(Ordering::Relaxed) {
            match self.queue.try_send(f) {
                Ok(()) => return,
                Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                    f = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => return,
            }
        }
    }
}

/// Serve the one request; the exit code it ended with (also sent, last).
pub fn serve() -> i32 {
    let (reply, finished) = Reply::start();
    let code = match answer(&reply) {
        Ok(code) => code,
        Err(Refusal { json, code, why }) => {
            // A caller that asked for JSON reads its error as JSON (`{"error": …}`), as the broker
            // writes it; a person reads text.
            if json {
                reply.send(Frame::Out(format!("{}\n", json!({ "error": why, "password": code == socket::NEED_PASSWORD }))));
            } else {
                reply.send(Frame::Err(format!("Error: {why}\n")));
            }
            code
        }
    };
    // The last frames go out to a caller still reading, for a while; then serve ends, whoever is
    // still at the other end.
    let deadline = Instant::now() + Duration::from_secs(socket::TIMEOUT_SECONDS);
    reply.send_last(Frame::Exit(code), deadline);
    drop(reply);
    let _ = finished.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    code
}

/// One frame to the caller, unless it has gone; a failed write means it has.
fn put(out: &mut impl Write, gone: &AtomicBool, f: &Frame) {
    if !gone.load(Ordering::Relaxed) && serde_json::to_string(f).map_or(true, |l| writeln!(out, "{l}").is_err()) {
        gone.store(true, Ordering::Relaxed);
    }
}

struct Refusal {
    json: bool,
    code: i32,
    why: String,
}

fn answer(reply: &Reply) -> std::result::Result<i32, Refusal> {
    let refuse = |json: bool, code: i32| move |e: anyhow::Error| Refusal { json, code, why: format!("{e:#}") };
    if !config::running_as_root() {
        return Err(refuse(false, 1)(anyhow::anyhow!("serve runs as root, from torq-broker.socket")));
    }
    let uid = rustix::net::sockopt::socket_peercred(std::io::stdin())
        .context("this is not a connection to the broker's socket (no SO_PEERCRED)")
        .map_err(refuse(false, 1))?
        .uid
        .as_raw();
    // A caller that sends nothing, or stops reading, holds nothing for long (the audit of ADR-040,
    // its 3 and 4): the request must come within the time, and a reply no one reads is dropped —
    // the broker's own output is still read, so a change never waits on its caller.
    let limit = Some(std::time::Duration::from_secs(socket::TIMEOUT_SECONDS));
    rustix::net::sockopt::set_socket_timeout(std::io::stdin(), rustix::net::sockopt::Timeout::Recv, limit)
        .and_then(|()| rustix::net::sockopt::set_socket_timeout(std::io::stdout(), rustix::net::sockopt::Timeout::Send, limit))
        .context("setting the connection's time limits")
        .map_err(refuse(false, 1))?;
    let req = read_request().map_err(refuse(false, 2))?;
    let json = req.args.iter().any(|a| a == "--json");
    let refused = refuse(json, 1);
    if uid == 0 {
        return Err(refused(anyhow::anyhow!("root runs torq-broker itself, not through the socket")));
    }
    let read = |p: &str| std::fs::read_to_string(p).with_context(|| format!("reading {p}"));
    let who = socket::caller(uid, &read("/etc/passwd").map_err(&refused)?, &read("/etc/group").map_err(&refused)?).map_err(&refused)?;
    socket::check_args(&req.args).map_err(&refused)?;
    let settings = Settings::read(Path::new(config::FIXED), false).map_err(&refused)?.context("no /etc/torq/broker.json").map_err(&refused)?;

    // The request's files, private to root, gone when this is.
    let scratch = Scratch::new(Path::new(SCRATCH)).map_err(&refused)?;
    let diff = req.diff.as_deref().map(|d| scratch.write("change.diff", d)).transpose().map_err(&refused)?;
    let hash = req.hash.as_deref().map(|h| scratch.write("hash", h)).transpose().map_err(&refused)?;
    let key = req.key.as_deref().map(|k| scratch.write("key", k)).transpose().map_err(&refused)?;
    let cmd = req.args.iter().find(|a| !a.starts_with('-')).cloned().unwrap_or_default();
    let diff_s = diff.as_ref().map(|p| p.to_string_lossy().into_owned());
    let hash_s = hash.as_ref().map(|p| p.to_string_lossy().into_owned());
    let key_s = key.as_ref().map(|p| p.to_string_lossy().into_owned());
    let args = socket::with_identity(&req.args, &cmd, &who, diff_s.as_deref(), hash_s.as_deref(), key_s.as_deref()).map_err(&refused)?;

    // The arguments as the broker will read them: what they name, for the gate.
    let m = crate::command_line()
        .try_get_matches_from(std::iter::once("torq-broker".to_string()).chain(args.iter().cloned()))
        .map_err(|e| Refusal { json, code: 2, why: e.render().to_string().trim().to_string() })?;
    let (name, sub) = m.subcommand().ok_or_else(|| refused(anyhow::anyhow!("no command")))?;
    if name != cmd {
        return Err(refused(anyhow::anyhow!("the command is {name}, not {cmd}")));
    }
    let seq = sub.try_get_one::<u64>("seq").ok().flatten().copied();
    // Whose a scheduled commit is: its approver may cancel it (the ledger's word, not the caller's).
    let user = match (name, seq) {
        ("cancel-scheduled", Some(seq)) => scheduled_by(&settings.state_dir, seq),
        _ => sub.try_get_one::<String>("user").ok().flatten().cloned(),
    };
    let change = match (name, seq) {
        ("apply" | "schedule" | "revert", Some(seq)) => change(&settings.state_dir, seq, if name == "revert" { "applied" } else { "proposal" }),
        _ => None,
    };
    // A commit confirmed holds everyone else's changes back while it waits: its own case.
    let confirmed = name == "apply" && sub.try_get_one::<u32>("confirm").ok().flatten().is_some();

    match socket::gate(&who, if confirmed { "apply-confirmed" } else { name }, change.as_ref(), user.as_deref()) {
        Gate::Open => {}
        Gate::Refused(why) => return Err(refused(anyhow::anyhow!(why))),
        Gate::Password => match req.password.as_deref() {
            None => return Err(Refusal { json, code: socket::NEED_PASSWORD, why: format!("this needs {}'s password", who.name) }),
            Some(p) => {
                // One check at a time per account: guesses sent at once wait their turn, and each
                // wrong one costs PAM's delay before the next is tried (its 1).
                let _turn = one_at_a_time(who.uid).map_err(&refused)?;
                if !socket::password_ok(&who.name, p) {
                    return Err(Refusal { json, code: socket::NEED_PASSWORD, why: format!("that is not {}'s password", who.name) });
                }
            }
        },
    }
    drop(req);
    Ok(run(reply, &who, &args))
}

/// The turn to check `uid`'s password: an exclusive lock on its file in faillock's directory
/// (root's alone), held until dropped.
fn one_at_a_time(uid: u32) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = Path::new(socket::FAILLOCK_DIR).join(format!(".turn-{uid}"));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&p)
        .with_context(|| format!("opening {}", p.display()))?;
    f.lock().context("waiting for the turn to check the password")?;
    Ok(f)
}

/// Who approved the commit scheduled as entry `seq`, if it is one.
fn scheduled_by(state_dir: &Path, seq: u64) -> Option<String> {
    let e = Ledger::open(state_dir.join("ledger.jsonl")).get(seq).ok()?;
    (e.body.kind == "apply_scheduled").then_some(e.body.approved_by).flatten()
}

/// The change `seq` is, if it is a `kind` entry: its tier and the files it touches.
fn change(state_dir: &Path, seq: u64, kind: &str) -> Option<Change> {
    let e = Ledger::open(state_dir.join("ledger.jsonl")).get(seq).ok()?;
    if e.body.kind != kind {
        return None;
    }
    let tier: Tier = serde_json::from_value(json!(e.body.tier)).ok()?;
    let files = e.body.data["files"].as_array()?.iter().filter_map(|f| f.as_str().map(String::from)).collect();
    Some(Change { tier, files })
}

/// One JSON line, no longer than the limit, all of it within the time: each read may wait only
/// for what is left of it, and the line ends at its first newline.
fn read_request() -> Result<Request> {
    use rustix::net::sockopt::{set_socket_timeout, Timeout};
    let deadline = Instant::now() + Duration::from_secs(socket::TIMEOUT_SECONDS);
    let mut stdin = std::io::stdin().take(socket::MAX_REQUEST);
    let (mut line, mut buf) = (Vec::new(), [0u8; 64 * 1024]);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("reading the request: it did not come within {} s", socket::TIMEOUT_SECONDS);
        }
        set_socket_timeout(std::io::stdin(), Timeout::Recv, Some(left.max(Duration::from_millis(1)))).context("reading the request")?;
        let n = stdin.read(&mut buf).context("reading the request")?;
        if n == 0 {
            bail!("the request is one JSON line, at most {} bytes", socket::MAX_REQUEST);
        }
        if let Some(end) = buf[..n].iter().position(|&b| b == b'\n') {
            line.extend_from_slice(&buf[..end]);
            break;
        }
        line.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&line).context("the request is not one the broker reads")
}

/// The broker itself, as root, with `args`: its output to the caller as it comes, its exit code.
fn run(reply: &Reply, who: &Caller, args: &[String]) -> i32 {
    let child = Command::new("/proc/self/exe")
        .args(args)
        // Nothing of serve's environment: the broker reads its settings from broker.json.
        .env_clear()
        .env("TORQ_VIA", "socket")
        // Who asked, as the kernel said: in every entry, inside what the hash covers (its 6).
        .env("TORQ_CALLER", &who.name)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            reply.send(Frame::Err(format!("Error: running the broker for {}: {e}\n", who.name)));
            return 1;
        }
    };
    // Standard error (the build's progress) may be left out when the caller falls behind; standard
    // output — the broker's answer, its JSON — never is: what does not fit waits, up to a limit, and
    // goes once the broker is done.
    let forward = |from: Option<Box<dyn Read + Send>>, frame: fn(String) -> Frame, keep: bool| {
        let reply = reply.clone();
        std::thread::spawn(move || {
            let mut held: Vec<Frame> = Vec::new();
            let mut held_bytes = 0usize;
            let Some(from) = from else { return held };
            let mut r = BufReader::new(from);
            let mut line = Vec::new();
            while r.read_until(b'\n', &mut line).is_ok_and(|n| n > 0) {
                let f = frame(String::from_utf8_lossy(&line).into_owned());
                if keep && (!held.is_empty() || !reply.offer(&f)) {
                    if held_bytes < KEEP_BYTES {
                        held_bytes += line.len();
                        held.push(f);
                    } else {
                        reply.skipped.fetch_add(1, Ordering::Relaxed);
                    }
                } else if !keep {
                    reply.send(f);
                }
                line.clear();
            }
            held
        })
    };
    let out = forward(child.stdout.take().map(|o| Box::new(o) as Box<dyn Read + Send>), Frame::Out, true);
    let err = forward(child.stderr.take().map(|e| Box::new(e) as Box<dyn Read + Send>), Frame::Err, false);
    let code = match child.wait() {
        Ok(s) => s.code().unwrap_or(1),
        Err(_) => 1,
    };
    // Its output, to its end — unless something it started still holds the pipes open: then a
    // while, and no longer (serve must end).
    let until = Instant::now() + Duration::from_secs(socket::TIMEOUT_SECONDS);
    while !(out.is_finished() && err.is_finished()) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    if out.is_finished() {
        for f in out.join().unwrap_or_default() {
            reply.send_last(f, until + Duration::from_secs(socket::TIMEOUT_SECONDS));
        }
    }
    code
}

/// A directory for one request's files (a diff, a hash, a backup's key): in `/run/torq`, memory only,
/// never on a disk or in a backup (the review of the backup, L2); 0700, root's; its own
/// name, removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(under: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let dir = under.join(format!("request-{}", std::process::id()));
        // A leftover of a server that died, with this pid: not ours to trust.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Scratch(dir))
    }

    fn write(&self, name: &str, text: &str) -> Result<PathBuf> {
        use std::os::unix::fs::OpenOptionsExt;
        let p = self.0.join(name);
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&p).with_context(|| format!("writing {}", p.display()))?;
        f.write_all(text.as_bytes())?;
        Ok(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
