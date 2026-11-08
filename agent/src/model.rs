//! The model, as an OpenAI-compatible chat endpoint (ADR-042): llama.cpp's `llama-server` on this
//! machine by default, over a unix socket; or a remote endpoint (another OpenAI-compatible server,
//! Anthropic's OpenAI-compatible API) when the owner sets one. Spoken with curl, the maintained
//! client, run by its store path with no configuration file of its own read (`-q`: the agent's home
//! is the agent's to write, and a `.curlrc` there could send the request elsewhere).

use crate::clean::clean;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where the model answers.
#[derive(Debug, PartialEq)]
pub enum Endpoint {
    /// `unix:/run/llama-cpp/llama.sock`: on this machine, a socket only the model's service can make.
    Unix(PathBuf),
    /// `http://127.0.0.1:8080/v1` (this machine only) or `https://…/v1`: the base URL, without its last `/`.
    Http { base: String, local: bool },
}

impl Endpoint {
    /// The endpoint the setting names. Plain http only to this machine: a key and what the tools
    /// returned are never sent in the clear.
    pub fn parse(s: &str) -> Result<Endpoint> {
        if let Some(path) = s.strip_prefix("unix:") {
            if !path.starts_with('/') {
                bail!("{s}: a unix socket by its absolute path (unix:/run/…)");
            }
            return Ok(Endpoint::Unix(PathBuf::from(path)));
        }
        // No userinfo, query, fragment or backslash anywhere: what curl takes for the host is then
        // what is checked here (`http://127.0.0.1:1@evil.example/v1` is evil.example to curl).
        if s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '#' | '?' | '\\')) {
            bail!("{s:?}: an endpoint is a plain URL (no user, query, fragment or backslash)");
        }
        let (rest, local) = if let Some(rest) = s.strip_prefix("https://") {
            (rest, false)
        } else if let Some(rest) = s.strip_prefix("http://") {
            (rest, true)
        } else {
            bail!("{s}: an endpoint is unix:/path, http://127.0.0.1:<port>/v1, or https://…/v1");
        };
        let authority = rest.split('/').next().unwrap_or("");
        if authority.is_empty() {
            bail!("{s}: no host");
        }
        if local {
            // The whole host[:port], compared: one of this machine's names, then at most a port.
            let ok = ["127.0.0.1", "localhost", "[::1]"].iter().any(|h| {
                authority.strip_prefix(h).is_some_and(|p| p.is_empty() || p.strip_prefix(':').is_some_and(|n| !n.is_empty() && n.len() <= 5 && n.bytes().all(|b| b.is_ascii_digit())))
            });
            if !ok {
                bail!("{s}: plain http only to this machine (127.0.0.1); another host needs https");
            }
        }
        Ok(Endpoint::Http { base: s.trim_end_matches('/').to_string(), local })
    }

    /// Whether what the model is shown stays on this machine.
    pub fn local(&self) -> bool {
        match self {
            Endpoint::Unix(_) => true,
            Endpoint::Http { local, .. } => *local,
        }
    }

    /// How it is named to the person.
    pub fn describe(&self) -> String {
        match self {
            Endpoint::Unix(p) => format!("unix:{}", p.display()),
            Endpoint::Http { base, .. } => base.clone(),
        }
    }

    /// curl's arguments for one request: no configuration file of its own (`-q`, first: the agent's
    /// home could hold a `.curlrc`), no redirect followed, no proxy, http only to this machine and
    /// https only elsewhere, an answer of at most 4 MiB within 600 s; the body on its input; the key's
    /// header, if any, from a file.
    fn curl_args(&self, header: Option<&Path>) -> Vec<String> {
        let mut a: Vec<String> = ["-q", "-sS", "--fail-with-body", "--max-time", "600", "--max-filesize", "4194304", "--noproxy", "*", "--proto"]
            .map(String::from)
            .to_vec();
        a.push(if self.local() { "=http" } else { "=https" }.into());
        a.extend(["-H", "Content-Type: application/json", "-H", "Expect:", "--data-binary", "@-"].map(String::from));
        if let Some(h) = header {
            a.extend(["-H".into(), format!("@{}", h.display())]);
        }
        match self {
            Endpoint::Unix(p) => a.extend(["--unix-socket".into(), p.display().to_string(), "http://localhost/v1/chat/completions".into()]),
            Endpoint::Http { base, .. } => a.push(format!("{base}/chat/completions")),
        }
        a
    }

    /// One chat completion: the request's JSON in, the response's JSON out.
    pub fn chat(&self, key: Option<&str>, body: &Value) -> Result<Value> {
        let curl = option_env!("TORQ_CURL").context("this build has no curl (TORQ_CURL; agent/package.nix, `nix develop`)")?;
        // The key in a private file (0600, in a /tmp the launcher makes the agent's own): never in argv.
        let header = match key {
            Some(k) => {
                let mut f = tempfile::NamedTempFile::new().context("a private file for the key's header")?;
                writeln!(f, "Authorization: Bearer {}", k.trim())?;
                Some(f)
            }
            None => None,
        };
        let mut cmd = Command::new(curl);
        cmd.args(self.curl_args(header.as_ref().map(|f| f.path())));
        let mut child = cmd
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running curl")?;
        let mut stdin = child.stdin.take().context("curl's input")?;
        stdin.write_all(serde_json::to_vec(body)?.as_slice())?;
        drop(stdin);
        let out = child.wait_with_output().context("waiting for curl")?;
        drop(header);
        if !out.status.success() {
            let why = format!("{} {}", String::from_utf8_lossy(&out.stderr), String::from_utf8_lossy(&out.stdout));
            bail!("the model at {} did not answer: {}", self.describe(), crate::tools::truncate(&clean(why.trim()), 600));
        }
        serde_json::from_slice(&out.stdout).with_context(|| format!("the model at {} answered something not JSON", self.describe()))
    }
}

/// The API key, from the file the setting names (outside the repo, as passwords are): none without one.
pub fn key(file: Option<&Path>) -> Result<Option<String>> {
    let Some(f) = file else { return Ok(None) };
    let k = std::fs::read_to_string(f).with_context(|| format!("reading the model's key from {}", f.display()))?;
    let k = k.trim().to_string();
    if k.is_empty() || k.chars().any(|c| c.is_control() || c.is_whitespace()) {
        bail!("{}: one key, on one line", f.display());
    }
    Ok(Some(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_local_or_encrypted() {
        assert_eq!(Endpoint::parse("unix:/run/llama-cpp/llama.sock").unwrap(), Endpoint::Unix("/run/llama-cpp/llama.sock".into()));
        let local = Endpoint::parse("http://127.0.0.1:8080/v1/").unwrap();
        assert_eq!(local, Endpoint::Http { base: "http://127.0.0.1:8080/v1".into(), local: true });
        assert!(Endpoint::parse("http://[::1]:8080/v1").unwrap().local());
        let remote = Endpoint::parse("https://api.anthropic.com/v1").unwrap();
        assert!(!remote.local());
        for bad in [
            "http://192.168.1.5:8080/v1", "http://127.0.0.1.evil.com/v1", "http://localhost@evil.com/v1",
            "http://127.0.0.1:1@evil.example/v1", "http://127.0.0.1:8080@evil.example/v1", "http://[::1]@evil.example/v1",
            "http://127.0.0.1:80x/v1", "http://127.0.0.1:/v1", "http://127.0.0.1\\@evil.example/v1", "http://127.0.0.1#@evil.example/v1",
            "http://127.0.0.1?@evil.example/v1", "https://user:pw@api.example/v1", "ftp://x", "unix:relative.sock", "https://", "https://a b",
        ] {
            assert!(Endpoint::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn curl_reads_no_configuration_and_speaks_only_its_protocol() {
        let local = Endpoint::parse("http://127.0.0.1:8080/v1").unwrap().curl_args(None);
        assert_eq!(local[0], "-q", "first, or curl reads ~/.curlrc");
        let at = |a: &[String], f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].clone());
        assert_eq!(at(&local, "--proto").as_deref(), Some("=http"));
        assert_eq!(at(&local, "--max-filesize").as_deref(), Some("4194304"));
        assert!(!local.iter().any(|x| x == "-L" || x == "--location"), "no redirect followed");
        let remote = Endpoint::parse("https://api.anthropic.com/v1").unwrap().curl_args(Some(Path::new("/tmp/h")));
        assert_eq!(remote[0], "-q");
        assert_eq!(at(&remote, "--proto").as_deref(), Some("=https"));
        assert_eq!(remote.last().unwrap(), "https://api.anthropic.com/v1/chat/completions");
        assert!(remote.windows(2).any(|w| w[0] == "-H" && w[1] == "@/tmp/h"), "the key from a file, never inline");
        let sock = Endpoint::parse("unix:/run/llama-cpp/llama.sock").unwrap().curl_args(None);
        assert_eq!(at(&sock, "--unix-socket").as_deref(), Some("/run/llama-cpp/llama.sock"));
        assert_eq!(at(&sock, "--proto").as_deref(), Some("=http"));
    }

    #[test]
    fn a_key_is_one_line() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("key");
        std::fs::write(&f, "sk-abc\n").unwrap();
        assert_eq!(key(Some(&f)).unwrap().as_deref(), Some("sk-abc"));
        std::fs::write(&f, "sk-abc\nX-Other: y\n").unwrap();
        assert!(key(Some(&f)).is_err(), "no second header smuggled in");
        assert_eq!(key(None).unwrap(), None);
    }
}
