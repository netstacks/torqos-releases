//! The request made, by curl: HTTPS only, the certificate checked against the system's, no redirect followed, no
//! proxy, nothing of curl's own configuration read. The value goes to curl on its standard input, in its
//! configuration, never in its arguments (which every account can read in /proc).

use crate::http::{Request, Route};
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// How long a request may take, and the most that comes back.
pub const SECONDS: u32 = 120;
pub const ANSWER_MAX: usize = 32 * 1024 * 1024;

/// What the host answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A value as curl's configuration quotes it.
fn quoted(v: &str) -> String {
    format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
}

/// curl's configuration for the request: where, how, its headers and the secret's, its body from `body`.
pub fn config(route: &Route, req: &Request, headers: &[(String, String)], secret: (&str, &str), body: Option<&std::path::Path>) -> String {
    let mut lines: Vec<(String, String)> = vec![("url".into(), format!("https://{}{}", route.host, route.rest))];
    // HEAD is curl's `head` flag: as a method, curl would wait for a body that never comes.
    if req.method != "HEAD" {
        lines.push(("request".into(), req.method.clone()));
    }
    lines.extend(headers.iter().map(|(k, v)| ("header".to_string(), format!("{k}: {v}"))));
    // No compression: what comes back is checked for the secret, which a compressed body would hide.
    lines.push(("header".into(), "Accept-Encoding: identity".into()));
    // No 100-continue: one request, one answer.
    lines.push(("header".into(), "Expect:".into()));
    lines.push(("header".into(), format!("{}: {}", secret.0, secret.1)));
    if let Some(b) = body {
        lines.push(("data-binary".into(), format!("@{}", b.display())));
    }
    for (k, v) in [("proto", "=https"), ("proto-redir", "=https"), ("noproxy", "*"), ("max-time", &SECONDS.to_string()), ("max-filesize", &ANSWER_MAX.to_string())] {
        lines.push((k.into(), v.into()));
    }
    let mut c: String = lines.iter().map(|(k, v)| format!("{k} = {}\n", quoted(v))).collect();
    // globoff: the URL is one URL, never a pattern of many ([1-5000] would send the secret 5000 times, as one use).
    c.push_str("silent\nshow-error\ninclude\ngloboff\n");
    if req.method == "HEAD" {
        c.push_str("head\n");
    }
    c
}

/// What curl wrote (`include`): the answer's head, after any 1xx ones, then its body.
pub fn parse(out: &[u8]) -> Result<Answer> {
    let mut rest = out;
    loop {
        // A head ends at an empty line; a host that ends its lines with a bare LF is read too.
        let (end, gap) = match (rest.windows(4).position(|w| w == b"\r\n\r\n"), rest.windows(2).position(|w| w == b"\n\n")) {
            (Some(a), Some(b)) if b < a => (b, 2),
            (Some(a), _) => (a, 4),
            (None, Some(b)) => (b, 2),
            (None, None) => bail!("the host's answer has no end to its head"),
        };
        let head = std::str::from_utf8(&rest[..end]).context("the host's answer's head is not text")?;
        rest = &rest[end + gap..];
        let mut lines = head.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
        let status_line = lines.next().unwrap_or_default();
        let mut parts = status_line.splitn(3, ' ');
        let (Some(version), Some(code)) = (parts.next(), parts.next()) else { bail!("the host's answer has no status") };
        if !version.starts_with("HTTP/") {
            bail!("the host's answer is not HTTP");
        }
        let status: u16 = code.parse().context("the host's status is not a number")?;
        if (100..200).contains(&status) {
            continue;
        }
        let reason = parts.next().unwrap_or("").to_string();
        let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
        return Ok(Answer { status, reason, headers, body: rest.to_vec() });
    }
}

/// The request, made: the host's answer, or why there is none.
pub fn send(route: &Route, req: &Request, headers: &[(String, String)], secret: (&str, &str)) -> Result<Answer> {
    // The body in a file of the daemon's own (its unit's private /tmp): curl's standard input carries its configuration.
    let body = (!req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH")).then(|| -> Result<Body> {
        use std::os::unix::fs::OpenOptionsExt;
        let mut bytes = [0u8; 8];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let path = std::env::temp_dir().join(format!("torq-secrets-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        // Held from here: written or not, the file goes.
        let held = Body(path);
        f.write_all(&req.body)?;
        Ok(held)
    });
    let body = body.transpose()?;
    let text = config(route, req, headers, secret, body.as_ref().map(|b| b.0.as_path()));
    let mut child = Command::new("curl")
        // --disable first: no .curlrc of anyone's. Nothing else on the command line.
        .args(["--disable", "--config", "-"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running curl")?;
    child.stdin.take().context("curl's input")?.write_all(text.as_bytes())?;
    drop(text);
    let mut out = Vec::new();
    child.stdout.take().context("curl's output")?.take((ANSWER_MAX + HEAD_ROOM) as u64 + 1).read_to_end(&mut out)?;
    // Too large: curl is stopped, not left writing to no one.
    if out.len() > ANSWER_MAX + HEAD_ROOM {
        let _ = child.kill();
        let _ = child.wait();
        bail!("the host's answer is larger than {} MiB", ANSWER_MAX >> 20);
    }
    let done = child.wait_with_output()?;
    if !done.status.success() {
        // Whole: the caller scrubs it before it cuts it (a value across a cut would be left half there).
        let why = String::from_utf8_lossy(&done.stderr[..done.stderr.len().min(64 * 1024)]).into_owned();
        bail!("the request did not go through: {}", why.trim());
    }
    parse(&out)
}

/// Room for the answer's head beside its body.
const HEAD_ROOM: usize = 1 << 20;

/// The body's file, gone when this is.
struct Body(std::path::PathBuf);

impl Drop for Body {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str) -> Request {
        Request { method: method.into(), target: "/system/ci/api.github.com/user".into(), headers: vec![], body: vec![] }
    }

    #[test]
    fn curls_configuration_says_everything_and_quotes_it() {
        let route = Route { user: None, name: "ci".into(), host: "api.github.com".into(), rest: "/user?q=\"x\"".into() };
        let c = config(&route, &req("GET"), &[("X-Note".into(), "a \"b\" \\c".into())], ("Authorization", "Bearer tok"), None);
        assert!(c.contains("url = \"https://api.github.com/user?q=\\\"x\\\"\"\n"), "{c}");
        assert!(c.contains("header = \"X-Note: a \\\"b\\\" \\\\c\"\n"), "{c}");
        assert!(c.contains("header = \"Authorization: Bearer tok\"\n") && c.contains("proto = \"=https\"\n") && c.contains("proto-redir = \"=https\"\n") && c.contains("noproxy = \"*\"\n"), "{c}");
        assert!(!c.contains("location"), "no redirect followed: curl follows none unless told");
        let lines: Vec<&str> = c.lines().collect();
        assert!(lines.iter().all(|l| !l.is_empty()), "one setting a line: nothing a header could add");
        let head = config(&route, &req("HEAD"), &[], ("A", "b"), None);
        assert!(head.ends_with("head\n") && !head.contains("request ="), "{head}");
    }

    #[test]
    fn the_answer_after_any_interim_ones() {
        let a = parse(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/2 201 \r\ncontent-type: application/json\r\nx-a: b: c\r\n\r\n{\"ok\":1}").unwrap();
        assert_eq!((a.status, a.body.as_slice()), (201, b"{\"ok\":1}".as_slice()));
        assert_eq!(a.headers, [("content-type".to_string(), "application/json".to_string()), ("x-a".to_string(), "b: c".to_string())]);
        assert!(parse(b"garbage").is_err());
        assert!(parse(b"SSH-2.0 x\r\n\r\n").is_err());
    }
}
