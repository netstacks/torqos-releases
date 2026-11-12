//! torq-secrets (ADR-052): a program on this machine uses a secret in an HTTPS request, and never reads it.
//!
//! It listens on 127.0.0.1:7800. A request to `/system/<secret>/<host>/<path>` (a person's own:
//! `/user/<person>/<secret>/<host>/<path>`) is asked of the broker as a use: the broker reads who is connected
//! from the kernel itself, judges the use by the secret's rule, records the agent's first use in each session,
//! and hands back the value for this one request. The daemon makes it with curl — HTTPS to that host and no other,
//! the certificate checked, no redirect followed — and gives back the answer with any form of the value in it
//! replaced. It runs as its own account, `torq-secrets`, which can read no value itself; it keeps none.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

pub mod agent;
pub mod http;
pub mod scrub;
pub mod upstream;

use anyhow::{bail, Context, Result};
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use torq_wire::secrets::PORT;
use torq_wire::socket::{Frame, Request as BrokerRequest, OK, SOCKET};

/// The most requests at once: past them, a request is turned away rather than queued.
const AT_ONCE: usize = 16;
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// `torq-secrets serve`, or its usage.
pub fn run() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("serve") => match serve() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("torq-secrets: {e:#}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: torq-secrets serve   (the secrets daemon, run by its unit: ADR-052)");
            ExitCode::from(2)
        }
    }
}

fn serve() -> Result<()> {
    use rustix::process::{set_dumpable_behavior, setrlimit, DumpableBehavior, Resource, Rlimit};
    // It holds values, one request at a time: nothing of its memory is ever read out or written to a core.
    set_dumpable_behavior(DumpableBehavior::NotDumpable).context("protecting the values")?;
    setrlimit(Resource::Core, Rlimit { current: Some(0), maximum: Some(0) }).context("protecting the values")?;
    let listener = TcpListener::bind(("127.0.0.1", PORT)).with_context(|| format!("listening on 127.0.0.1:{PORT}"))?;
    eprintln!("torq-secrets: listening on 127.0.0.1:{PORT}");
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if RUNNING.fetch_add(1, Ordering::SeqCst) >= AT_ONCE {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.write_all(&http::refusal(503, "too many requests at once: try again"));
            continue;
        }
        // A thread that cannot be had (the unit's TasksMax) is a request turned away, not a daemon that stops.
        let spawned = std::thread::Builder::new().name("request".into()).spawn(move || {
            let answer = one(&stream);
            let _ = stream.write_all(&answer);
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            RUNNING.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// What the broker granted.
#[derive(serde::Deserialize)]
struct Granted {
    value: String,
    header: String,
    prefix: String,
    actor: String,
}

/// Ask the broker for a use (`secret-use`): what it granted, or why not. The value is in this answer, and in
/// nothing kept.
fn ask_broker(route: &http::Route, client_port: u16, session: Option<&str>) -> Result<std::result::Result<Granted, String>> {
    let mut args = vec!["--json".to_string(), "secret-use".into(), "--name".into(), route.name.clone(), "--host".into(), route.host.clone()];
    args.extend(["--client-port".into(), client_port.to_string(), "--server-port".into(), PORT.to_string()]);
    if let Some(u) = &route.user {
        args.extend(["--user".into(), u.clone()]);
    }
    if let Some(s) = session {
        args.extend(["--session".into(), s.to_string()]);
    }
    broker(args)
}

/// One request of the broker's (the daemon's account may ask only `secret-use` and `secret-agent`): its answer as
/// `T`, or why it refused.
fn broker<T: serde::de::DeserializeOwned>(args: Vec<String>) -> Result<std::result::Result<T, String>> {
    use std::io::BufRead;
    let mut s = std::os::unix::net::UnixStream::connect(SOCKET).with_context(|| format!("reaching the broker at {SOCKET}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
    writeln!(s, "{}", serde_json::to_string(&BrokerRequest { args, ..Default::default() })?)?;
    let (mut out, mut code) = (String::new(), None);
    for line in BufReader::new(s).lines() {
        match serde_json::from_str::<Frame>(&line.context("reading the broker's answer")?) {
            Ok(Frame::Out(t)) => out.push_str(&t),
            Ok(Frame::Exit(c)) => code = Some(c),
            Ok(Frame::Err(_)) => {}
            Err(_) => break,
        }
    }
    match code {
        Some(OK) => Ok(Ok(serde_json::from_str(&out).context("the broker's answer")?)),
        Some(_) => {
            let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
            Ok(Err(v["error"].as_str().unwrap_or("refused").to_string()))
        }
        None => bail!("the broker hung up"),
    }
}

/// An SSH use, for a socket's key (`agent.rs`): judged by the broker by the socket's grant; the value and the host's keys.
fn ask_ssh(key: &agent::Key, grant: &str, host: &str) -> Result<std::result::Result<(String, Vec<String>), String>> {
    #[derive(serde::Deserialize)]
    struct Use {
        value: String,
        known: Vec<String>,
    }
    let mut args = vec!["--json".to_string(), "secret-use".into(), "--ssh".into(), "--name".into(), key.name.clone(), "--host".into(), host.into(), "--grant".into(), grant.into()];
    if let Some(u) = key.owner.get("person").and_then(|p| p.as_str()) {
        args.extend(["--user".into(), u.into()]);
    }
    Ok(broker::<Use>(args)?.map(|u| (u.value, u.known)))
}

/// `POST /ssh-agent`: an agent socket for the one connected, granted by the broker (who, read from the kernel).
fn agent_socket(req: &http::Request, client_port: u16) -> Vec<u8> {
    let mut args = vec!["--json".to_string(), "secret-agent".into(), "--client-port".into(), client_port.to_string(), "--server-port".into(), PORT.to_string()];
    if let Some(s) = req.header(torq_wire::secrets::SESSION_HEADER) {
        args.extend(["--session".into(), s.to_string()]);
    }
    let g: agent::Granted = match broker(args) {
        Ok(Ok(g)) => g,
        Ok(Err(why)) => return http::refusal(403, &why),
        Err(e) => return http::refusal(503, &format!("{e:#}")),
    };
    let (actor, keys) = (g.actor.clone(), g.keys.iter().map(|k| k.name.clone()).collect::<Vec<_>>());
    match agent::open(g, ask_ssh) {
        Ok(path) => {
            log(&format!("an agent socket for {actor}, with {} key(s)", keys.len()));
            let body = format!("{}\n", serde_json::json!({ "socket": path, "keys": keys, "seconds": 3600 }));
            http::response(200, "OK", &[("Content-Type".into(), "application/json".into())], body.as_bytes())
        }
        Err(e) => http::refusal(503, &format!("{e:#}")),
    }
}

/// How long a program has to send its whole request: one deadline, not a time between bytes, so a connection
/// that sends a byte at a time cannot hold its place for ever.
const REQUEST_SECONDS: u64 = 15;

/// A connection read against one deadline.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: std::time::Instant,
}

impl std::io::Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the request took too long"));
        }
        self.stream.set_read_timeout(Some(left))?;
        (&mut &*self.stream).read(buf)
    }
}

/// One connection's answer.
fn one(stream: &TcpStream) -> Vec<u8> {
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(upstream::SECONDS as u64)));
    let Ok(peer) = stream.peer_addr() else { return http::refusal(400, "no peer") };
    let until = std::time::Instant::now() + std::time::Duration::from_secs(REQUEST_SECONDS);
    let req = match http::read(&mut BufReader::new(Deadline { stream, until })) {
        Ok(r) => r,
        Err(why) => return http::refusal(400, &why),
    };
    if let Err(why) = http::not_a_browser(&req) {
        log(&format!("refused a browser's request: {why}"));
        return http::refusal(403, &why);
    }
    // An agent socket for SSH (agent.rs): asked as a use is, so that the broker reads who from the kernel.
    if req.target == "/ssh-agent" {
        if req.method != "POST" {
            return http::refusal(400, "an agent socket is asked for with POST /ssh-agent");
        }
        return agent_socket(&req, peer.port());
    }
    let route = match http::route(&req.target) {
        Ok(r) => r,
        Err(why) => return http::refusal(400, &why),
    };
    let granted = match ask_broker(&route, peer.port(), req.header(torq_wire::secrets::SESSION_HEADER)) {
        Ok(Ok(g)) => g,
        Ok(Err(why)) => {
            log(&format!("refused: {} {} for {}: {why}", req.method, route.host, whose(&route)));
            return http::refusal(403, &why);
        }
        Err(e) => return http::refusal(503, &format!("{e:#}")),
    };
    // A value that could end a header line would add headers of its own: never sent.
    if granted.value.chars().any(|c| c == '\r' || c == '\n' || c == '\0') || granted.prefix.chars().any(|c| c.is_control()) {
        return http::refusal(502, &format!("{} holds a line break: it cannot go in a header", whose(&route)));
    }
    let headers = http::forwarded(&req, &granted.header);
    let header_value = format!("{}{}", granted.prefix, granted.value);
    let forms = scrub::forms(&granted.value);
    let sent = upstream::send(&route, &req, &headers, (&granted.header, &header_value));
    drop(header_value);
    let clean = |s: &str| -> (String, bool) {
        let (b, f) = scrub::scrub(s.as_bytes(), &forms);
        (String::from_utf8_lossy(&b).into_owned(), f)
    };
    let path = route.rest.split('?').next().unwrap_or("/");
    let answer = match sent {
        Err(e) => {
            log(&format!("{} used {} for {} https://{}{path}: failed", granted.actor, whose(&route), req.method, route.host));
            // Scrubbed whole, then cut: a value across the cut is not left half there.
            let why: String = clean(&format!("{e:#}")).0.chars().filter(|c| !c.is_control()).take(300).collect();
            return http::refusal(502, &why);
        }
        Ok(a) => a,
    };
    if answer.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("content-encoding") && !v.eq_ignore_ascii_case("identity")) {
        return http::refusal(502, "the host answered compressed, though asked not to: a compressed answer cannot be checked for the secret");
    }
    // HTTP's own between hops, and what would let a web page read the answer (CORS): never passed on. A HEAD's
    // length is the host's, as HEAD says what a GET would be; any other's is the daemon's own.
    const HOP: &[&str] = &["connection", "keep-alive", "transfer-encoding", "content-length", "trailer", "upgrade", "proxy-authenticate"];
    let head = req.method == "HEAD";
    let mut found = false;
    let mut out_headers = Vec::new();
    for (k, v) in &answer.headers {
        let low = k.to_ascii_lowercase();
        let keep_length = head && low == "content-length";
        if (HOP.contains(&low.as_str()) && !keep_length) || low.starts_with("access-control-") || !http::token_ok(k) || v.chars().any(|c| c.is_control() && c != '\t') {
            continue;
        }
        let (k, f1) = clean(k);
        let (v, f2) = clean(v);
        found |= f1 || f2;
        out_headers.push((k, v));
    }
    let (body, f) = scrub::scrub(&answer.body, &forms);
    found |= f;
    let (reason, f) = clean(&answer.reason);
    found |= f;
    let reason: String = reason.chars().filter(|c| !c.is_control()).collect();
    log(&format!(
        "{} used {} for {} https://{}{path}: {}{}",
        granted.actor,
        whose(&route),
        req.method,
        route.host,
        answer.status,
        if found { " (the host sent the secret back: replaced)" } else { "" }
    ));
    if head {
        return http::head_response(answer.status, &reason, &out_headers);
    }
    http::response(answer.status, &reason, &out_headers, &body)
}

fn whose(r: &http::Route) -> String {
    match &r.user {
        None => format!("the system's secret {}", r.name),
        Some(u) => format!("{u}'s secret {}", r.name),
    }
}

/// Every use, to the journal (the daemon's log, ADR-052 3): who, which secret, what, and how it went. Never a value.
pub(crate) fn log(line: &str) {
    eprintln!("{}", line.chars().filter(|c| !c.is_control()).collect::<String>());
}
