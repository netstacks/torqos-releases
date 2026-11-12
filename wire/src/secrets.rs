//! The rules of a machine's secrets (ADR-052): what each may be used for, by whom, and when a person is asked.
//! Two files of the configuration hold them, in this one shape: `secrets/system.json`, an administrator's, and
//! `users/<person>/secrets.json`, the person's own. The values are never here: the broker keeps them outside
//! the repository. The broker checks a file's shape before it is committed, torqcli writes it, and the daemon
//! reads it to decide each use. All three use this module, so a word one of them knows they all know.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Where the system's rules are, in the configuration repository.
pub const SYSTEM_RULES: &str = "secrets/system.json";
/// A person's own rules: `users/<person>/` and this.
pub const PERSON_RULES: &str = "secrets.json";
/// The agent's account (ADR-041): the only actor besides people.
pub const AGENT: &str = "torq-agent";
/// The daemon that uses secrets (`torq-secrets`, its own account): the only caller of the broker's `secret-use`.
pub const DAEMON: &str = "torq-secrets";
/// Where the daemon listens: one loopback port, a path for each use (`/system/<secret>/<host>/…`,
/// `/user/<person>/<secret>/<host>/…`).
pub const PORT: u16 = 7800;
/// The header an agent's request names its session in.
pub const SESSION_HEADER: &str = "x-torq-session";

/// The most secrets one file names, and the most hosts one use names.
pub const MAX_SECRETS: usize = 64;
pub const MAX_HOSTS: usize = 16;
/// The most a rules file may hold, in bytes.
pub const MAX_FILE: u64 = 64 * 1024;

/// One file's rules: each secret by name.
pub type Rules = BTreeMap<String, Rule>;

/// What a secret is, which decides what it can be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A token or a password sent in an HTTPS header.
    Token,
    /// An SSH private key: for SSH and for signing, never sent anywhere.
    SshKey,
}

/// When a person is asked before the agent uses the secret. A tainted session is always asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Approve {
    EachUse,
    #[default]
    FirstUse,
    Never,
}

impl Approve {
    pub fn word(self) -> &'static str {
        match self {
            Approve::EachUse => "each-use",
            Approve::FirstUse => "first-use",
            Approve::Never => "never",
        }
    }

    pub fn from_word(w: &str) -> Option<Approve> {
        [Approve::EachUse, Approve::FirstUse, Approve::Never].into_iter().find(|a| a.word() == w)
    }
}

/// An HTTPS use: the hosts it may be sent to, and the header it goes in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Https {
    pub hosts: Vec<String>,
    /// The header's name (`Authorization`).
    #[serde(default = "authorization")]
    pub header: String,
    /// What comes before the value in it (`Bearer `): may be empty.
    #[serde(default = "bearer")]
    pub prefix: String,
}

fn authorization() -> String {
    "Authorization".into()
}

fn bearer() -> String {
    "Bearer ".into()
}

/// An SSH use: the hosts the key may sign for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ssh {
    pub hosts: Vec<String>,
    /// Each host's keys, as known_hosts writes them (`<host> <type> <base64>`): what OpenSSH's agent checks the
    /// server against before it signs. Taken from a person's own known_hosts when they allow the host, never on
    /// first sight of it.
    #[serde(default)]
    pub known: Vec<String>,
}

/// One secret's rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub https: Option<Https>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Ssh>,
    /// Who may use it besides its owner: `torq-agent`; for a system secret, people too (`parse_person` refuses them in a person's).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actors: Vec<String>,
    #[serde(default)]
    pub approve: Approve,
}

impl Rule {
    /// A new secret's rule: nobody's use yet.
    pub fn new(kind: Kind) -> Rule {
        Rule { kind, https: None, ssh: None, actors: Vec::new(), approve: Approve::default() }
    }
}

/// A secret's name: letters, digits, `-` and `_`, as it names a file. At most 64.
pub fn name_ok(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && !n.starts_with('-') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A host a secret may go to: a DNS name, lowercase, with at least one dot (no `localhost`, no bare name a
/// search domain could finish), no address and no wildcard. An address is refused: a rule names who the
/// certificate is for, not where it happens to be.
pub fn host_ok(h: &str) -> bool {
    let label = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let labels: Vec<&str> = h.split('.').collect();
    h.len() <= 253 && labels.len() >= 2 && labels.iter().all(|l| label(l)) && !labels.last().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit()))
}

/// An HTTP header's name: a token (RFC 9110), never one HTTP itself decides with.
fn header_ok(h: &str) -> bool {
    const HTTP_S_OWN: &[&str] = &["host", "content-length", "transfer-encoding", "connection", "upgrade", "te", "trailer", "proxy-authorization", "cookie"];
    !h.is_empty() && h.len() <= 64 && h.chars().all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)) && !HTTP_S_OWN.contains(&h.to_ascii_lowercase().as_str())
}

/// What precedes the value in its header: printable, no line break.
fn prefix_ok(p: &str) -> bool {
    p.len() <= 32 && p.chars().all(|c| c == ' ' || c.is_ascii_graphic())
}

/// An account that may use a secret: the agent, or a person's name.
fn actor_ok(a: &str) -> bool {
    a == AGENT || (!a.is_empty() && a.len() <= 32 && a.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
}

/// A host's key, as known_hosts writes one for port 22: `<host> <type> <base64>`, the host one of `hosts`, no
/// option, no comment, nothing hashed (a rule says which host in the clear: it is read by the person allowing it).
pub fn known_ok(line: &str, hosts: &[String]) -> bool {
    const TYPES: &[&str] = &["ssh-ed25519", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384", "ecdsa-sha2-nistp521", "ssh-rsa", "sk-ssh-ed25519@openssh.com", "sk-ecdsa-sha2-nistp256@openssh.com"];
    let f: Vec<&str> = line.split(' ').collect();
    let [host, kind, key] = f.as_slice() else { return false };
    hosts.iter().any(|h| h == host) && TYPES.contains(kind) && (16..=16384).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
}

fn hosts_ok(hosts: &[String]) -> Result<(), String> {
    if hosts.is_empty() || hosts.len() > MAX_HOSTS {
        return Err(format!("a use names 1 to {MAX_HOSTS} hosts"));
    }
    if let Some(h) = hosts.iter().find(|h| !host_ok(h)) {
        return Err(format!("{h:?} is not a host name (lowercase, with a dot; no address, no wildcard)"));
    }
    if let Some(h) = hosts.iter().enumerate().find_map(|(i, h)| hosts[..i].contains(h).then_some(h)) {
        return Err(format!("{h} is named twice"));
    }
    Ok(())
}

/// Why `rules` is not a rules file, or None.
pub fn check(rules: &Rules) -> Result<(), String> {
    if rules.len() > MAX_SECRETS {
        return Err(format!("at most {MAX_SECRETS} secrets"));
    }
    for (name, r) in rules {
        let at = |why: String| format!("secret {name}: {why}");
        if !name_ok(name) {
            return Err(format!("{name:?} is not a secret's name (letters, digits, - and _)"));
        }
        match (r.kind, &r.https, &r.ssh) {
            (Kind::Token, _, Some(_)) => return Err(at("a token is not an SSH key: it has no ssh use".into())),
            (Kind::SshKey, Some(_), _) => return Err(at("an SSH key is never sent: it has no https use".into())),
            _ => {}
        }
        if let Some(h) = &r.https {
            hosts_ok(&h.hosts).map_err(at)?;
            if !header_ok(&h.header) {
                return Err(at(format!("{:?} is not a header it may go in", h.header)));
            }
            if !prefix_ok(&h.prefix) {
                return Err(at(format!("{:?} is not a header's prefix", h.prefix)));
            }
        }
        if let Some(s) = &r.ssh {
            hosts_ok(&s.hosts).map_err(at)?;
            if s.known.len() > MAX_HOSTS * 8 {
                return Err(at("too many host keys".into()));
            }
            if let Some(l) = s.known.iter().find(|l| !known_ok(l, &s.hosts)) {
                return Err(at(format!("{l:?} is not a key of one of its hosts, as known_hosts writes it")));
            }
            if let Some(h) = s.hosts.iter().find(|h| !s.known.iter().any(|l| l.split(' ').next() == Some(h.as_str()))) {
                return Err(at(format!("no key of {h} is known: ssh {h} once at a terminal, check its key, then allow it")));
            }
        }
        if r.actors.len() > MAX_SECRETS {
            return Err(at("too many actors".into()));
        }
        if let Some(a) = r.actors.iter().find(|a| !actor_ok(a)) {
            return Err(at(format!("{a:?} is not an account")));
        }
    }
    Ok(())
}

/// A rules file read and checked.
pub fn parse(bytes: &[u8]) -> Result<Rules, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(format!("larger than {} KiB", MAX_FILE / 1024));
    }
    let Unique(rules) = serde_json::from_slice(bytes).map_err(|e| format!("not a rules file: {e}"))?;
    check(&rules)?;
    Ok(rules)
}

/// A person's own rules file: as `parse`, and only the agent besides them may use their secrets. Another
/// person is given a secret as a system secret, by an administrator.
pub fn parse_person(bytes: &[u8]) -> Result<Rules, String> {
    let rules = parse(bytes)?;
    if let Some((name, r)) = rules.iter().find(|(_, r)| r.actors.iter().any(|a| a != AGENT)) {
        let other = r.actors.iter().find(|a| *a != AGENT).cloned().unwrap_or_default();
        return Err(format!("secret {name}: {other:?} is not the agent: a person's secret is used by them, and the agent if they say so"));
    }
    Ok(rules)
}

/// The rules, each secret named once. JSON lets a name come twice and serde keeps the last: a reviewer would read
/// the first and every program the last, so a name twice is no rules file at all.
struct Unique(Rules);

impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Names;
        impl<'de> serde::de::Visitor<'de> for Names {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("the secrets, by name")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Unique, A::Error> {
                let mut out = Rules::new();
                while let Some((name, rule)) = m.next_entry::<String, Rule>()? {
                    if out.contains_key(&name) {
                        return Err(serde::de::Error::custom(format!("secret {name} is named twice")));
                    }
                    out.insert(name, rule);
                }
                Ok(Unique(out))
            }
        }
        d.deserialize_map(Names)
    }
}

/// A rules file as it is written: sorted, two-space indented, ending in a newline.
pub fn render(rules: &Rules) -> String {
    let mut s = serde_json::to_string_pretty(rules).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// Standard base64, as an SSH key carries it (no line breaks; padding optional).
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

fn base64_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() { A[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// A string of SSH's wire format (uint32 length, then the bytes) at the start of `b`: it, and what follows.
pub fn ssh_string(b: &[u8]) -> Option<(&[u8], &[u8])> {
    let n = u32::from_be_bytes(b.get(..4)?.try_into().ok()?) as usize;
    let s = b.get(4..4usize.checked_add(n)?)?;
    Some((s, &b[4 + n..]))
}

/// The public half of an OpenSSH private key (`-----BEGIN OPENSSH PRIVATE KEY-----`), as a `.pub` file says it
/// (`<type> <base64>`), read from the key's own header, where it is in the clear: no program is given the private
/// key to say it, and no file holds it. A key under a passphrase, or another format, is refused, saying why.
pub fn openssh_public(value: &str) -> Result<String, String> {
    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";
    let v = value.trim();
    if !v.starts_with(BEGIN) {
        return Err(if v.contains("PRIVATE KEY-----") {
            "not an OpenSSH key (an older PEM one): `ssh-keygen -p -f <key>` writes it in OpenSSH's form".into()
        } else {
            "not an SSH private key".into()
        });
    }
    let body: String = v.trim_start_matches(BEGIN).split(END).next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect();
    let raw = base64_decode(&body).ok_or("the key is not base64")?;
    let rest = raw.strip_prefix(b"openssh-key-v1\0".as_slice()).ok_or("the key is not openssh-key-v1")?;
    let (cipher, rest) = ssh_string(rest).ok_or("the key is cut short")?;
    if cipher != b"none" {
        return Err("the key has a passphrase: a secret is kept encrypted already, so give it one without (`ssh-keygen -p -N \"\" -f <a copy>`)".into());
    }
    let (_kdf, rest) = ssh_string(rest).ok_or("the key is cut short")?;
    let (_opts, rest) = ssh_string(rest).ok_or("the key is cut short")?;
    let count = u32::from_be_bytes(rest.get(..4).ok_or("the key is cut short")?.try_into().map_err(|_| "the key is cut short")?);
    if count != 1 {
        return Err("a file of one key, not several".into());
    }
    let (blob, _) = ssh_string(&rest[4..]).ok_or("the key is cut short")?;
    let (kind, _) = ssh_string(blob).ok_or("the key's public half is cut short")?;
    let kind = std::str::from_utf8(kind).map_err(|_| "the key's type is not text")?;
    if !kind.chars().all(|c| c.is_ascii_alphanumeric() || "-@.".contains(c)) {
        return Err("the key's type is not one".into());
    }
    Ok(format!("{kind} {}", base64_encode(blob)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(json: &str) -> Result<Rules, String> {
        parse(json.as_bytes())
    }

    #[test]
    fn a_rule_as_written() {
        let r = one(r#"{"github-ci":{"kind":"token","https":{"hosts":["api.github.com"]},"actors":["torq-agent"]}}"#).unwrap();
        let g = &r["github-ci"];
        assert_eq!(g.approve, Approve::FirstUse, "asked the first time, unless the rule says otherwise");
        let h = g.https.as_ref().unwrap();
        assert_eq!((h.header.as_str(), h.prefix.as_str()), ("Authorization", "Bearer "));
        assert_eq!(parse(render(&r).as_bytes()).unwrap(), r, "what is written reads back the same");
    }

    #[test]
    fn a_use_goes_only_where_it_can_be_named() {
        for bad in ["localhost", "github", "10.0.0.1", "1.2.3.4", "*.github.com", "API.github.com", "a..b", "-a.com", "a.com.", "exa mple.com", "evil.com/x", "a.com:443"] {
            assert!(!host_ok(bad), "{bad}");
        }
        for good in ["api.github.com", "git.example.org", "a1.b-c.io"] {
            assert!(host_ok(good), "{good}");
        }
        assert!(one(r#"{"t":{"kind":"token","https":{"hosts":[]}}}"#).is_err(), "a use names a host");
    }

    #[test]
    fn a_secret_is_used_only_as_what_it_is() {
        assert!(one(r#"{"t":{"kind":"token","ssh":{"hosts":["github.com"]}}}"#).is_err());
        assert!(one(r#"{"k":{"kind":"ssh-key","https":{"hosts":["api.github.com"]}}}"#).is_err());
        assert!(one(r#"{"k":{"kind":"ssh-key","ssh":{"hosts":["github.com"]}}}"#).is_err(), "no key of the host known");
        assert!(one(r#"{"k":{"kind":"ssh-key","ssh":{"hosts":["github.com"],"known":["github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl"]}}}"#).is_ok());
        for bad in [
            "gitlab.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMq",
            "github.com ssh-dss AAAAB3NzaC1kc3MAAACBAOMqqnkV",
            "github.com ssh-ed25519 AAAA C3Nz",
            "|1|hashed= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMq",
            "github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMq comment",
            "@revoked github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMq",
        ] {
            assert!(!known_ok(bad, &["github.com".into()]), "{bad}");
        }
    }

    #[test]
    fn nothing_odd_is_taken() {
        for bad in [
            r#"{"t":{"kind":"token","shell":true}}"#,
            r#"{"t":{"kind":"password"}}"#,
            r#"{"../x":{"kind":"token"}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"header":"Host"}}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"header":"X-A\r\nB"}}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com"],"prefix":"a\nb"}}}"#,
            r#"{"t":{"kind":"token","actors":["Root"]}}"#,
            r#"{"t":{"kind":"token","approve":"sometimes"}}"#,
            r#"[]"#,
        ] {
            assert!(one(bad).is_err(), "{bad}");
        }
        assert!(parse(&vec![b' '; MAX_FILE as usize + 1]).is_err());
    }

    /// What a reviewer reads is what every program reads: nothing named twice.
    #[test]
    fn nothing_is_named_twice() {
        for twice in [
            r#"{"t":{"kind":"token"},"t":{"kind":"ssh-key"}}"#,
            r#"{"t":{"kind":"token","kind":"ssh-key"}}"#,
            r#"{"t":{"kind":"token","https":{"hosts":["a.com","a.com"]}}}"#,
        ] {
            assert!(one(twice).is_err(), "{twice}");
        }
    }

    #[test]
    fn a_persons_secret_is_theirs_and_the_agents_at_most() {
        assert!(parse_person(br#"{"t":{"kind":"token","actors":["torq-agent"]}}"#).is_ok());
        assert!(parse_person(br#"{"t":{"kind":"token","actors":["sam"]}}"#).is_err());
        assert!(parse(br#"{"t":{"kind":"token","actors":["sam"]}}"#).is_ok(), "a system secret may be a person's to use");
    }

    /// A throwaway key, made for this test alone (ssh-keygen -t ed25519 -N ""), and its public half as ssh-keygen wrote it.
    const TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\nQyNTUxOQAAACALEJlaQgiCLSgzMKjXIAECACR/bjD9FbMY71yZLrLm3AAAAIjyZMAb8mTA\nGwAAAAtzc2gtZWQyNTUxOQAAACALEJlaQgiCLSgzMKjXIAECACR/bjD9FbMY71yZLrLm3A\nAAAECXsWvw0VEbFBpa7vFncHfWTTRsPvpgrZQU6IYMrdmZMAsQmVpCCIItKDMwqNcgAQIA\nJH9uMP0VsxjvXJkusubcAAAABHRlc3QB\n-----END OPENSSH PRIVATE KEY-----";
    const TEST_PUB: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAsQmVpCCIItKDMwqNcgAQIAJH9uMP0VsxjvXJkusubc";
    const TEST_KEY_PASSPHRASE: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABBBz7TCRp\nqxE77REhi+I6auAAAAIAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIJi8NxsfM8PIPeSi\nDH2BhaL5F+kiWpAYBq5QrqpCetmtAAAAkBCutpUgJK6Cgx45e1nbTrKM1RwUMwNcr6e4JO\nibQhzoOaOz2zA6qbAopFdLLhZfCpNp44x5wlifs3DWXzEjP2FsFSpXUe3V75Hc0ueGhLpi\nZzA3DVq8w3DEMNAyTr7CH8PdDVxilz+IqwGZ9WPdNy3o9k429o+zbyxUuSRlxbeDVmgE3h\nD4KffEuUBf43mTQQ==\n-----END OPENSSH PRIVATE KEY-----";

    #[test]
    fn a_keys_public_half_from_its_own_header() {
        assert_eq!(openssh_public(TEST_KEY).unwrap(), TEST_PUB, "as ssh-keygen -y says it");
        assert_eq!(openssh_public(&format!("\n{TEST_KEY}\n\n")).unwrap(), TEST_PUB);
        assert!(openssh_public(TEST_KEY_PASSPHRASE).unwrap_err().contains("passphrase"));
        assert!(openssh_public("-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----").unwrap_err().contains("PEM"));
        assert!(openssh_public("ghp_token").is_err());
        assert!(openssh_public("-----BEGIN OPENSSH PRIVATE KEY-----\nb3Blbg==\n-----END OPENSSH PRIVATE KEY-----").is_err(), "cut short");
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        assert_eq!(base64_decode("a"), None);
    }
    #[test]
    fn names() {
        assert!(name_ok("github-ci") && name_ok("a_1") && !name_ok("") && !name_ok("-x") && !name_ok("a/b") && !name_ok(".x") && !name_ok(&"a".repeat(65)));
        assert_eq!(Approve::from_word("each-use"), Some(Approve::EachUse));
        assert_eq!(Approve::from_word("x"), None);
    }
}
