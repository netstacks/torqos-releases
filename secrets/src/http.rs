//! The little of HTTP/1.1 the daemon takes from a program on this machine (ADR-052): one request, its head and a
//! body of a stated length, and where it is for. Anything else is refused, never guessed at: no chunked body, no
//! folded header, no line that is not one.

use torq_wire::secrets::{host_ok, name_ok};

/// The most a request's head may be, its number of headers, and its body.
pub const HEAD_MAX: usize = 64 * 1024;
pub const HEADERS_MAX: usize = 100;
pub const BODY_MAX: usize = 32 * 1024 * 1024;

/// The methods passed on. CONNECT and TRACE are not (one makes a tunnel, the other echoes the request back), nor
/// OPTIONS: a browser asks it before a page's request, and no program needs it.
const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// The header `name`'s value (any case), if it is given once; given twice or more, None.
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut found = self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name));
        let first = found.next()?;
        found.next().is_none().then_some(first.1.as_str())
    }
}

/// A header's name: a token (RFC 9110).
pub fn token_ok(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c))
}

/// One request from `r`.
pub fn read(r: &mut impl std::io::BufRead) -> Result<Request, String> {
    use std::io::BufRead as _;
    let mut lines = Vec::new();
    let mut size = 0;
    loop {
        let mut line = Vec::new();
        let n = std::io::Read::take(&mut *r, (HEAD_MAX - size + 1) as u64).read_until(b'\n', &mut line).map_err(|e| format!("reading the request: {e}"))?;
        size += n;
        if n == 0 || size > HEAD_MAX {
            return Err(if n == 0 { "the request ended before its head did".into() } else { format!("a request's head is at most {} KiB", HEAD_MAX / 1024) });
        }
        let Some(line) = line.strip_suffix(b"\r\n") else { return Err("a line of the request does not end in CRLF".into()) };
        let line = std::str::from_utf8(line).map_err(|_| "a line of the request is not text".to_string())?;
        if line.chars().any(|c| c.is_control() && c != '\t') {
            return Err("a line of the request holds a control character".into());
        }
        if line.is_empty() {
            break;
        }
        lines.push(line.to_string());
    }
    let (first, rest) = lines.split_first().ok_or("no request line")?;
    let parts: Vec<&str> = first.split(' ').collect();
    let [method, target, version] = parts.as_slice() else { return Err("the request line is not METHOD TARGET VERSION".into()) };
    if !METHODS.contains(method) {
        return Err(format!("{method} is not passed on (one of {})", METHODS.join(", ")));
    }
    if !matches!(*version, "HTTP/1.1" | "HTTP/1.0") {
        return Err(format!("{version} is not taken: HTTP/1.1"));
    }
    if rest.len() > HEADERS_MAX {
        return Err(format!("at most {HEADERS_MAX} headers"));
    }
    let mut headers = Vec::new();
    for l in rest {
        if l.starts_with([' ', '\t']) {
            return Err("a folded header is not taken".into());
        }
        let (name, value) = l.split_once(':').ok_or("a header without a colon")?;
        if !token_ok(name) {
            return Err(format!("{name:?} is not a header's name"));
        }
        headers.push((name.to_string(), value.trim_matches([' ', '\t']).to_string()));
    }
    let mut req = Request { method: method.to_string(), target: target.to_string(), headers, body: Vec::new() };
    if req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding")) {
        return Err("a chunked body is not taken: send its Content-Length".into());
    }
    let lengths: Vec<&str> = req.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.as_str()).collect();
    let length = match lengths.as_slice() {
        [] => 0,
        [one] if !one.is_empty() && one.bytes().all(|b| b.is_ascii_digit()) => one.parse::<usize>().map_err(|_| "a Content-Length too large".to_string())?,
        _ => return Err("one Content-Length, a number".into()),
    };
    if length > BODY_MAX {
        return Err(format!("a body is at most {} MiB", BODY_MAX >> 20));
    }
    req.body = vec![0; length];
    r.read_exact(&mut req.body).map_err(|_| "the body ended before its Content-Length".to_string())?;
    Ok(req)
}

/// Why a browser's request is not taken (Err), if it is one. A web page the person has open runs as them: its
/// script can send a request to 127.0.0.1, and the broker would rightly see the person's own account. So the
/// daemon takes only what a program sends: addressed to the daemon by its own name (a page that rebinds its own
/// name to 127.0.0.1 sends that name, not this one), and with none of the headers a browser adds to what a page
/// sends (Origin, Referer, and the Fetch Metadata that every current browser sets).
pub fn not_a_browser(req: &Request) -> Result<(), String> {
    let host = req.header("host").unwrap_or_default();
    let ours = [format!("127.0.0.1:{}", torq_wire::secrets::PORT), format!("localhost:{}", torq_wire::secrets::PORT)];
    if !ours.iter().any(|h| h.eq_ignore_ascii_case(host)) {
        return Err(format!("a request is addressed to {} (its Host), not {host:?}", ours[0]));
    }
    if let Some((k, _)) = req.headers.iter().find(|(k, _)| {
        let k = k.to_ascii_lowercase();
        k == "origin" || k == "referer" || k.starts_with("sec-fetch-")
    }) {
        return Err(format!("a web page's request ({k}) is not taken: only programs use secrets here"));
    }
    Ok(())
}

/// Where a request is for: whose secret, its name, the host it goes to, and the path there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// A person's own secret (None: the system's).
    pub user: Option<String>,
    pub name: String,
    pub host: String,
    /// The path and query at the host, from its `/`.
    pub rest: String,
}

fn person_ok(p: &str) -> bool {
    !p.is_empty() && p.len() <= 32 && p.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// `/system/<secret>/<host>/…` or `/user/<person>/<secret>/<host>/…`.
pub fn route(target: &str) -> Result<Route, String> {
    let usage = || format!("{target:?} is not /system/<secret>/<host>/… nor /user/<person>/<secret>/<host>/…");
    if !target.starts_with('/') || target.contains('#') {
        return Err(usage());
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let (user, after) = if let Some(after) = path.strip_prefix("/system/") {
        (None, after)
    } else if let Some((person, after)) = path.strip_prefix("/user/").and_then(|r| r.split_once('/')) {
        if !person_ok(person) {
            return Err(usage());
        }
        (Some(person.to_string()), after)
    } else {
        return Err(usage());
    };
    let mut parts = after.splitn(3, '/');
    let (Some(name), Some(host)) = (parts.next(), parts.next()) else { return Err(usage()) };
    let tail = parts.next().unwrap_or("");
    if !name_ok(name) || !host_ok(host) {
        return Err(usage());
    }
    let mut rest = format!("/{tail}");
    if let Some(q) = query {
        rest.push('?');
        rest.push_str(q);
    }
    Ok(Route { user, name: name.to_string(), host: host.to_string(), rest })
}

/// The headers that go on to the host: not HTTP's own between this machine and the daemon, not the session's
/// name, not the one the secret goes in (the daemon sets it, no one else), and asking for no compression, so that
/// what comes back can be checked for the secret.
pub fn forwarded(req: &Request, secret_header: &str) -> Vec<(String, String)> {
    const DROPPED: &[&str] = &["host", "connection", "keep-alive", "te", "trailer", "transfer-encoding", "upgrade", "content-length", "expect", "accept-encoding", "proxy-authorization", "proxy-connection"];
    req.headers
        .iter()
        .filter(|(k, _)| {
            let k = k.to_ascii_lowercase();
            !DROPPED.contains(&k.as_str()) && k != torq_wire::secrets::SESSION_HEADER && !k.starts_with("proxy-") && !k.eq_ignore_ascii_case(secret_header)
        })
        .cloned()
        .collect()
}

/// A whole response, its length stated, the connection closed after it.
pub fn response(status: u16, reason: &str, headers: &[(String, String)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out
}

/// A HEAD's answer: its headers as the host gave them, its length among them, and no body.
pub fn head_response(status: u16, reason: &str, headers: &[(String, String)]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    for (k, v) in headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out
}

/// The daemon's own answer: an error, as JSON.
pub fn refusal(status: u16, why: &str) -> Vec<u8> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let body = format!("{}\n", serde_json::json!({ "error": why }));
    response(status, reason, &[("Content-Type".into(), "application/json".into())], body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Result<Request, String> {
        read(&mut std::io::BufReader::new(text.as_bytes()))
    }

    #[test]
    fn a_request_as_curl_sends_it() {
        let r = one("POST /user/sam/gh/api.github.com/repos/x?per_page=1 HTTP/1.1\r\nHost: 127.0.0.1:7800\r\nUser-Agent: curl\r\nX-Torq-Session: s\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((r.method.as_str(), r.body.as_slice()), ("POST", b"{}".as_slice()));
        assert_eq!(r.header("x-torq-session"), Some("s"));
        let route = route(&r.target).unwrap();
        assert_eq!(route, Route { user: Some("sam".into()), name: "gh".into(), host: "api.github.com".into(), rest: "/repos/x?per_page=1".into() });
        let names: Vec<String> = forwarded(&r, "Authorization").into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, ["User-Agent"], "not Host, the length, nor the session's name");
    }

    #[test]
    fn where_it_is_for() {
        assert_eq!(route("/system/ci/api.github.com").unwrap().rest, "/");
        assert_eq!(route("/system/ci/api.github.com/a/b/c").unwrap().rest, "/a/b/c");
        assert_eq!(route("/user/sam/gh/api.github.com/a/b?q=1/2").unwrap().rest, "/a/b?q=1/2");
        for bad in ["/system/ci", "/other/ci/api.github.com/", "/system/../api.github.com/", "/system/ci/10.0.0.1/", "/system/ci/localhost/", "/user/Sam/gh/a.com/", "/user/sam/gh/", "system/ci/a.com/", "/system/ci/a.com/#x", "/system/ci/a.com:443/"] {
            assert!(route(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_secrets_header_is_the_daemons_alone() {
        let r = one("GET /system/ci/a.com/ HTTP/1.1\r\nauthorization: Bearer mine\r\nAccept-Encoding: gzip\r\nProxy-Foo: x\r\nAccept: */*\r\n\r\n").unwrap();
        let kept: Vec<String> = forwarded(&r, "Authorization").into_iter().map(|(k, _)| k).collect();
        assert_eq!(kept, ["Accept"], "the program's own Authorization is dropped: the secret's goes in its place");
    }

    /// A web page the person has open sends as them: what a browser sends is not taken, nor a request for a name
    /// a page rebound to 127.0.0.1.
    #[test]
    fn a_browsers_request_is_not_a_programs() {
        let with = |extra: &str| one(&format!("GET /system/ci/api.github.com/ HTTP/1.1\r\nHost: 127.0.0.1:7800\r\n{extra}\r\n")).unwrap();
        assert!(not_a_browser(&with("")).is_ok(), "curl's");
        assert!(not_a_browser(&one("GET / HTTP/1.1\r\nHost: localhost:7800\r\n\r\n").unwrap()).is_ok());
        for extra in ["Origin: https://evil.example\r\n", "Referer: https://evil.example/x\r\n", "Sec-Fetch-Site: cross-site\r\n", "Sec-Fetch-Mode: no-cors\r\n", "sec-fetch-dest: image\r\n"] {
            assert!(not_a_browser(&with(extra)).is_err(), "{extra}");
        }
        for host in ["evil.example:7800", "127.0.0.1", "127.0.0.1:80", "", "127.0.0.2:7800"] {
            let r = one(&format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n")).unwrap();
            assert!(not_a_browser(&r).is_err(), "{host:?}");
        }
        assert!(not_a_browser(&one("GET / HTTP/1.1\r\n\r\n").unwrap()).is_err(), "no Host at all");
        assert!(not_a_browser(&one("GET / HTTP/1.1\r\nHost: 127.0.0.1:7800\r\nHost: evil.example\r\n\r\n").unwrap()).is_err(), "two");
        assert!(one("OPTIONS / HTTP/1.1\r\nHost: 127.0.0.1:7800\r\n\r\n").is_err(), "a browser's preflight");
    }

    #[test]
    fn nothing_odd_is_taken() {
        for bad in [
            "GET / HTTP/1.1\nHost: x\n\n",
            "CONNECT a.com:443 HTTP/1.1\r\n\r\n",
            "TRACE / HTTP/1.1\r\n\r\n",
            "GET / HTTP/2\r\n\r\n",
            "GET /  HTTP/1.1\r\n\r\n",
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab",
            "POST / HTTP/1.1\r\nContent-Length: -1\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab",
            "GET / HTTP/1.1\r\nX: a\r\n folded\r\n\r\n",
            "GET / HTTP/1.1\r\nBad Name: a\r\n\r\n",
            "GET / HTTP/1.1\r\nX: a\rb\r\n\r\n",
            "GET / HTTP/1.1\r\n",
        ] {
            assert!(one(bad).is_err(), "{bad:?}");
        }
        assert!(one(&format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(HEAD_MAX))).is_err(), "too long a head");
        assert!(one(&format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", BODY_MAX + 1)).is_err(), "too large a body");
        let twice = one("GET / HTTP/1.1\r\nX-Torq-Session: a\r\nx-torq-session: b\r\n\r\n").unwrap();
        assert_eq!(twice.header("x-torq-session"), None, "named twice is not named");
    }
}
