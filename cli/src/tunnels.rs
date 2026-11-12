//! A person's SSH tunnels (ADR-050): theirs, as a Wi-Fi network is, defined in their own home
//! (`~/.config/torq/tunnels.toml`), kept up by their own systemd (`torq-tunnel@<name>.service`), and shown
//! in the panel by `torq-tunnels`. Nothing here is privileged or touches the configuration: it reads and
//! writes the person's file, works out the one ssh command line a tunnel is, and asks the person's own
//! user manager to start, stop or remember one.
//!
//! The file holds a name, a destination and forwards, and nothing else: no free ssh options, so it cannot
//! be made to run a command (`ProxyCommand`, `LocalCommand`), and a destination or host that starts with
//! `-` is refused, as anything ssh could read as an option is. Forwards listen on 127.0.0.1 only; host
//! keys are never accepted on first use.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::process::Command;

/// The longest name: a systemd instance name, kept short enough to read in a panel.
const NAME_MAX: usize = 32;

/// ssh's options, the same for every tunnel (ADR-050 2 and 3). The person's `~/.ssh/config` is still read
/// (keys, `User`, `ProxyJump`); these come first on the command line, so they win over it.
pub const SSH_OPTIONS: &[&str] = &[
    // Never ask anything: no password prompt, no host key question. A tunnel either connects or says why.
    "BatchMode=yes",
    // A host key not already in known_hosts does not connect, and is not added (no `accept-new`).
    "StrictHostKeyChecking=yes",
    // Nor are a known server's other keys added behind the person's back (OpenSSH's default otherwise).
    "UpdateHostKeys=no",
    // A forward that cannot listen ends the connection, so systemd tries again and the panel says why.
    "ExitOnForwardFailure=yes",
    // A dead link is noticed within 45 s: ssh exits, systemd starts it again.
    "ServerAliveInterval=15",
    "ServerAliveCountMax=3",
    "ConnectTimeout=15",
    // Forwards listen on this machine's loopback only, whatever the person's ssh config says.
    "GatewayPorts=no",
    "PermitLocalCommand=no",
    // Its own connection, not a share of a multiplexed one that could close under it.
    "ControlMaster=no",
    "ControlPath=none",
    // The far end gets a tunnel, not the person's keys or display.
    "ForwardAgent=no",
    "ForwardX11=no",
];

/// One forward, in the file's words: `socks <port>`, `local <port> <host>:<port>`, `remote <port> <host>:<port>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forward {
    /// A SOCKS5 proxy on this machine's 127.0.0.1:<port>; what it connects to, and its DNS, from the far end.
    Socks(u16),
    /// This machine's 127.0.0.1:<port>, to <host>:<port> as the far end reaches it.
    Local(u16, String, u16),
    /// The far end's 127.0.0.1:<port>, to <host>:<port> as this machine reaches it.
    Remote(u16, String, u16),
}

impl fmt::Display for Forward {
    /// As the file writes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Forward::Socks(p) => write!(f, "socks {p}"),
            Forward::Local(p, h, hp) => write!(f, "local {p} {h}:{hp}"),
            Forward::Remote(p, h, hp) => write!(f, "remote {p} {h}:{hp}"),
        }
    }
}

impl Forward {
    /// As the panel says it.
    pub fn describe(&self) -> String {
        match self {
            Forward::Socks(p) => format!("SOCKS on 127.0.0.1:{p}"),
            Forward::Local(p, h, hp) => format!("127.0.0.1:{p} to {h}:{hp}"),
            Forward::Remote(p, h, hp) => format!("its 127.0.0.1:{p} to {h}:{hp} here"),
        }
    }

    /// The port this machine listens on, for a forward that listens here.
    pub fn listens_here(&self) -> Option<u16> {
        match self {
            Forward::Socks(p) | Forward::Local(p, ..) => Some(*p),
            Forward::Remote(..) => None,
        }
    }

    /// ssh's own option for it, bound to loopback.
    fn ssh(&self) -> [String; 2] {
        match self {
            Forward::Socks(p) => ["-D".into(), format!("127.0.0.1:{p}")],
            Forward::Local(p, h, hp) => ["-L".into(), format!("127.0.0.1:{p}:{h}:{hp}")],
            Forward::Remote(p, h, hp) => ["-R".into(), format!("127.0.0.1:{p}:{h}:{hp}")],
        }
    }

    /// One forward from the file's words.
    pub fn parse(words: &str) -> Result<Forward> {
        let w: Vec<&str> = words.split_whitespace().collect();
        let usage = || format!("`{words}` is not a forward: write `socks <port>`, `local <port> <host>:<port>` or `remote <port> <host>:<port>`");
        match w.as_slice() {
            ["socks", p] => Ok(Forward::Socks(port(p)?)),
            [kind @ ("local" | "remote"), p, to] => {
                let (h, hp) = to.rsplit_once(':').with_context(usage)?;
                let (h, hp) = (host(h)?, port(hp)?);
                Ok(if *kind == "local" { Forward::Local(port(p)?, h, hp) } else { Forward::Remote(port(p)?, h, hp) })
            }
            _ => bail!(usage()),
        }
    }
}

/// A port, 1 to 65535.
fn port(s: &str) -> Result<u16> {
    match s.parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => bail!("`{s}` is not a port (1 to 65535)"),
    }
}

/// A host a forward goes to: a name or an address, or an IPv6 address in brackets. Never an option.
fn host(s: &str) -> Result<String> {
    let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_');
    let ok = if let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        !inner.is_empty() && inner.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
    } else {
        !s.is_empty() && s.chars().all(plain)
    };
    if !ok || s.starts_with('-') {
        bail!("`{s}` is not a host name or address");
    }
    Ok(s.to_string())
}

/// A tunnel's name: a systemd instance, and what the panel shows.
fn name(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= NAME_MAX
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-');
    if !ok {
        bail!("`{s}` is not a tunnel name: lower-case letters, digits and `-`, up to {NAME_MAX}, not starting with `-`");
    }
    Ok(())
}

/// Where ssh connects: `user@host`, a host, or a `Host` of the person's ssh config. Never an option, and
/// nothing ssh would read as a URI (`ssh://`) or a second argument.
fn destination(s: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 255
        && !s.starts_with('-')
        && s.matches('@').count() <= 1
        && !s.starts_with('@')
        && !s.ends_with('@')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@' | ':' | '%'));
    if !ok {
        bail!("`{s}` is not a destination: write `user@host`, a host, or a Host of your ssh config");
    }
    Ok(())
}

/// One tunnel, checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunnel {
    pub name: String,
    pub destination: String,
    pub port: Option<u16>,
    pub forwards: Vec<Forward>,
}

/// One tunnel as the file has it. Anything else in it (an ssh option) is refused, not ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    destination: String,
    port: Option<u16>,
    forwards: Vec<String>,
}

impl Tunnel {
    /// Its systemd unit, `torq-tunnel@<name>.service`.
    pub fn unit(&self) -> String {
        unit(&self.name)
    }

    /// The whole ssh command line after `ssh`: the fixed options, the forwards, then `--` and the
    /// destination, so nothing after the options can be read as one.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec!["-N".into(), "-T".into()];
        for o in SSH_OPTIONS {
            a.push("-o".into());
            a.push((*o).into());
        }
        if let Some(p) = self.port {
            a.push("-p".into());
            a.push(p.to_string());
        }
        for f in &self.forwards {
            a.extend(f.ssh());
        }
        a.push("--".into());
        a.push(self.destination.clone());
        a
    }

    /// What it does, in a line: "cwdavis@192.168.50.45: SOCKS on 127.0.0.1:1080".
    pub fn describe(&self) -> String {
        let f: Vec<String> = self.forwards.iter().map(Forward::describe).collect();
        format!("{}: {}", self.destination, f.join(", "))
    }
}

/// The unit of a tunnel of this name.
pub fn unit(name: &str) -> String {
    format!("torq-tunnel@{name}.service")
}

/// A file read tunnel by tunnel: one wrong tunnel is said, and the others still work (a typo in one must not
/// stop the rest, which would otherwise each refuse at their next restart).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct File {
    /// The tunnels that are right, by name.
    pub tunnels: Vec<Tunnel>,
    /// Each wrong tunnel's name and why.
    pub wrong: Vec<(String, String)>,
    /// Two tunnels that listen on one port here: both are kept (only one can be up at a time), and it is said.
    pub clashes: Vec<String>,
}

/// A file's tunnels, each checked on its own. Only a file that is not TOML at all is an error: then no
/// tunnel of it can be read.
pub fn read(text: &str) -> Result<File> {
    let raw: BTreeMap<String, toml::Value> = toml::from_str(text).map_err(|e| anyhow::anyhow!("{}", e.message()))?;
    let mut f = File::default();
    for (n, v) in raw {
        match v.try_into::<Raw>().map_err(|e| anyhow::anyhow!("{}", e.message())).and_then(|r| check(&n, r)) {
            Ok(t) => f.tunnels.push(t),
            Err(e) => f.wrong.push((n, format!("{e:#}"))),
        }
    }
    let mut seen: BTreeMap<u16, &str> = BTreeMap::new();
    for t in &f.tunnels {
        for p in t.forwards.iter().filter_map(Forward::listens_here) {
            if let Some(other) = seen.insert(p, &t.name) {
                f.clashes.push(format!("tunnels `{other}` and `{}` both listen on 127.0.0.1:{p}", t.name));
            }
        }
    }
    Ok(f)
}

/// Every tunnel of a file's text, sorted by name, the file refused whole if any of it is wrong: what
/// `torq-tunnel check` and "Add tunnel" hold a file to, so the person sees what to fix.
pub fn parse(text: &str) -> Result<Vec<Tunnel>> {
    let f = read(text)?;
    if let Some((n, why)) = f.wrong.first() {
        bail!("tunnel `{n}`: {why}");
    }
    if let Some(c) = f.clashes.first() {
        bail!("{c}");
    }
    Ok(f.tunnels)
}

fn check(n: &str, r: Raw) -> Result<Tunnel> {
    name(n)?;
    destination(&r.destination)?;
    if r.port == Some(0) {
        bail!("`0` is not a port (1 to 65535)");
    }
    if r.forwards.is_empty() {
        bail!("it has no forwards: add one, such as `forwards = [\"socks 1080\"]`");
    }
    let forwards = r.forwards.iter().map(|f| Forward::parse(f)).collect::<Result<Vec<_>>>()?;
    Ok(Tunnel { name: n.to_string(), destination: r.destination, port: r.port, forwards })
}

/// The file: `$XDG_CONFIG_HOME/torq/tunnels.toml`, or `~/.config/torq/tunnels.toml`.
pub fn file() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("torq/tunnels.toml"))
}

/// What a new file says before any tunnel is in it.
pub const TEMPLATE: &str = "# Your SSH tunnels (TorqOS, ADR-050). Each is kept up while you are logged in, once started from
# the panel's SSH tunnels applet. The host's key must already be known: run `ssh <destination>` once.
#
# [example]
# destination = \"user@host\"          # or a Host of your ~/.ssh/config
# port = 22                           # optional
# forwards = [\"socks 1080\"]           # socks <port> | local <port> <host>:<port> | remote <port> <host>:<port>
";

/// The person's tunnels, each checked on its own; none if there is no file yet.
pub fn load() -> Result<File> {
    let path = file()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => read(&text).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(File::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The file whole, refused if any of it is wrong (`torq-tunnel check`).
pub fn load_strict() -> Result<Vec<Tunnel>> {
    let path = file()?;
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).with_context(|| format!("{}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// The file's text with one more tunnel at its end, checked whole: what is already there is kept as the
/// person wrote it, comments included.
pub fn add(text: &str, n: &str, dest: &str, forwards: &[String]) -> Result<String> {
    name(n)?;
    destination(dest)?;
    if parse(text)?.iter().any(|t| t.name == n) {
        bail!("there is already a tunnel `{n}`");
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    // Each forward checked first, and written in its own words: only letters, digits, `.:-_[] ` reach the file.
    let list: Vec<String> = forwards.iter().map(|f| Forward::parse(f).map(|f| format!("\"{f}\""))).collect::<Result<_>>()?;
    out.push_str(&format!("[{n}]\ndestination = {dest:?}\nforwards = [{}]\n", list.join(", ")));
    parse(&out)?;
    Ok(out)
}

/// Adds a tunnel to the person's file, made if it is not there, readable by them alone.
pub fn add_to_file(n: &str, dest: &str, forwards: &[String]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = file()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TEMPLATE.to_string(),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let new = add(&text, n, dest, forwards)?;
    // A file that is a link (a dotfiles manager's) is written where it points, and stays a link.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let dir = path.parent().context("the file has no folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".tunnels.toml.new");
    // A leftover from an interrupted save would keep its own mode: made new, readable by the person alone.
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(new.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Makes the file, with its template, if it is not there: for "Edit tunnels…".
pub fn ensure_file() -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = file()?;
    if !path.exists() {
        std::fs::create_dir_all(path.parent().context("the file has no folder")?)?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path)?;
        f.write_all(TEMPLATE.as_bytes())?;
    }
    Ok(path)
}

/// A tunnel's state, for the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not started.
    Off,
    /// Running, its forwards not listening yet.
    Connecting,
    /// Running, and listening where it should.
    Up,
    /// Meant to be up, not up: systemd tries again. Why, in a line.
    Retrying(String),
    /// Stopped by itself and not tried again (a wrong file). Why, in a line.
    Failed(String),
}

impl State {
    /// Up or on its way: what the switch shows as on.
    pub fn on(&self) -> bool {
        matches!(self, State::Up | State::Connecting | State::Retrying(_))
    }
}

/// A tunnel's state from its unit's (`ActiveState`, `SubState`), whether its forwards listen, and the
/// last thing it said.
pub fn state(active: &str, sub: &str, listening: bool, last: Option<&str>, t: &Tunnel) -> State {
    let why = || explain(last.unwrap_or(""), t);
    match (active, sub) {
        ("active", "running") if listening => State::Up,
        ("active", "running") => State::Connecting,
        ("activating", _) | ("deactivating", _) | (_, "auto-restart") => State::Retrying(why()),
        ("failed", _) => State::Failed(why()),
        _ => State::Off,
    }
}

/// What ssh's last line means, in the person's words. The first match wins; anything else is said as is.
pub fn explain(line: &str, t: &Tunnel) -> String {
    let l = line.to_ascii_lowercase();
    let host = t.destination.rsplit('@').next().unwrap_or(&t.destination);
    if l.contains("host key verification failed") || l.contains("host key is known") || l.contains("no matching host key") {
        format!("The key of {host} is not known. Run `ssh {}` once in a terminal and check it.", t.destination)
    } else if l.contains("remote host identification has changed") {
        format!("The key of {host} has changed since you last connected. Check why before trusting it again.")
    } else if l.contains("permission denied") {
        format!("{host} refused your key. Is it in your keyring (`ssh-add -l`)?")
    } else if l.contains("address already in use") || l.contains("cannot listen") || l.contains("port forwarding failed") {
        "A port it listens on is in use. Retrying.".to_string()
    } else if l.contains("could not resolve") || l.contains("unreachable") || l.contains("timed out") || l.contains("connection refused") || l.contains("closed by remote") || l.contains("broken pipe") || l.contains("timeout") {
        format!("Cannot reach {host}. Retrying.")
    } else if line.trim().is_empty() {
        "Retrying.".to_string()
    } else {
        line.trim().to_string()
    }
}

/// Whether 127.0.0.1:<port> is listening here (from /proc/net/tcp, no program asked).
pub fn listening(port: u16) -> bool {
    let want = format!("0100007F:{port:04X}");
    std::fs::read_to_string("/proc/net/tcp")
        .map(|t| t.lines().skip(1).any(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            f.get(1) == Some(&want.as_str()) && f.get(3) == Some(&"0A")
        }))
        .unwrap_or(false)
}

/// Each tunnel with its state and whether it starts at login, from the person's user manager.
pub fn status(all: &[Tunnel]) -> Vec<(State, bool)> {
    let units: Vec<String> = all.iter().map(Tunnel::unit).collect();
    let mut shown: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    if !units.is_empty() {
        if let Ok(o) = Command::new("systemctl").args(["--user", "show", "--property=Id,ActiveState,SubState,UnitFileState"]).args(&units).output() {
            for block in String::from_utf8_lossy(&o.stdout).split("\n\n") {
                let kv: BTreeMap<String, String> = block.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
                if let Some(id) = kv.get("Id") {
                    shown.insert(id.clone(), kv);
                }
            }
        }
    }
    all.iter()
        .map(|t| {
            let kv = shown.get(&t.unit());
            let get = |k: &str| kv.and_then(|m| m.get(k)).map(String::as_str).unwrap_or("");
            let ports: Vec<u16> = t.forwards.iter().filter_map(Forward::listens_here).collect();
            let up = ports.iter().all(|p| listening(*p));
            let (active, sub) = (get("ActiveState"), get("SubState"));
            // ssh's last word, only when it matters.
            let last = if (active == "active" && sub == "running") || active == "inactive" || active.is_empty() { None } else { last_line(&t.unit()) };
            (state(active, sub, up, last.as_deref(), t), get("UnitFileState") == "enabled")
        })
        .collect()
}

/// The last thing the tunnel itself said (ssh, or torq-tunnel before it), from the person's journal: only
/// its own process's lines (`_SYSTEMD_USER_UNIT`), not the user manager's about it ("Failed with result…").
fn last_line(unit: &str) -> Option<String> {
    let o = Command::new("journalctl").args(["--user", &format!("_SYSTEMD_USER_UNIT={unit}"), "--lines=1", "--output=cat", "--no-pager", "--quiet"]).output().ok()?;
    String::from_utf8_lossy(&o.stdout).lines().last().map(str::to_string)
}

/// The person's user manager, asked: `systemctl --user <verb> <unit>`.
fn systemctl(args: &[&str]) -> Result<()> {
    let o = Command::new("systemctl").arg("--user").args(args).output().context("cannot run systemctl")?;
    if !o.status.success() {
        bail!("{}", String::from_utf8_lossy(&o.stderr).trim());
    }
    Ok(())
}

/// Starts a tunnel now (and forgets a failure, so it is tried at once).
pub fn start(name: &str) -> Result<()> {
    let u = unit(name);
    let _ = systemctl(&["reset-failed", &u]);
    systemctl(&["start", &u])
}

/// Stops a tunnel now.
pub fn stop(name: &str) -> Result<()> {
    systemctl(&["stop", &unit(name)])
}

/// Whether a tunnel starts at login (`systemctl --user enable`): remembered in the person's own home.
pub fn at_login(name: &str, on: bool) -> Result<()> {
    systemctl(&[if on { "enable" } else { "disable" }, &unit(name)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> Tunnel {
        parse(text).unwrap().remove(0)
    }

    #[test]
    fn a_tunnel_is_its_name_destination_and_forwards() {
        let t = one("[viasat]\ndestination = \"cwdavis@192.168.50.45\"\nforwards = [\"socks 1080\", \"local 8080 intranet:80\", \"remote 2222 localhost:22\"]\n");
        assert_eq!(t.name, "viasat");
        assert_eq!(t.forwards, vec![Forward::Socks(1080), Forward::Local(8080, "intranet".into(), 80), Forward::Remote(2222, "localhost".into(), 22)]);
        assert_eq!(t.unit(), "torq-tunnel@viasat.service");
        assert_eq!(t.describe(), "cwdavis@192.168.50.45: SOCKS on 127.0.0.1:1080, 127.0.0.1:8080 to intranet:80, its 127.0.0.1:2222 to localhost:22 here");
    }

    #[test]
    fn the_command_line_is_fixed_options_forwards_then_the_destination() {
        let t = one("[a]\ndestination = \"me@host\"\nport = 2200\nforwards = [\"socks 1080\", \"local 8080 [::1]:80\", \"remote 9000 localhost:22\"]\n");
        let a = t.ssh_args();
        assert_eq!(&a[..2], ["-N", "-T"]);
        // Every option, by name: one dropped or weakened fails here.
        let want = [
            "BatchMode=yes", "StrictHostKeyChecking=yes", "UpdateHostKeys=no", "ExitOnForwardFailure=yes",
            "ServerAliveInterval=15", "ServerAliveCountMax=3", "ConnectTimeout=15", "GatewayPorts=no",
            "PermitLocalCommand=no", "ControlMaster=no", "ControlPath=none", "ForwardAgent=no", "ForwardX11=no",
        ];
        assert_eq!(SSH_OPTIONS, want);
        let given: Vec<&str> = a[2..2 + 2 * want.len()].chunks(2).map(|w| { assert_eq!(w[0], "-o"); w[1].as_str() }).collect();
        assert_eq!(given, want);
        assert!(!a.iter().any(|x| x.contains("accept-new") || x.starts_with("ProxyCommand") || x.starts_with("LocalCommand")));
        let tail: Vec<&str> = a[2 + 2 * SSH_OPTIONS.len()..].iter().map(String::as_str).collect();
        assert_eq!(tail, ["-p", "2200", "-D", "127.0.0.1:1080", "-L", "127.0.0.1:8080:[::1]:80", "-R", "127.0.0.1:9000:localhost:22", "--", "me@host"]);
    }

    #[test]
    fn every_forward_listens_on_loopback_only() {
        let t = one("[a]\ndestination = \"h\"\nforwards = [\"socks 1\", \"local 2 x:3\", \"remote 4 y:5\"]\n");
        let a = t.ssh_args();
        let binds: Vec<&String> = a.windows(2).filter(|w| matches!(w[0].as_str(), "-D" | "-L" | "-R")).map(|w| &w[1]).collect();
        assert_eq!(binds.len(), 3);
        assert!(binds.iter().all(|b| b.starts_with("127.0.0.1:")));
    }

    #[test]
    fn nothing_in_the_file_becomes_an_ssh_option() {
        // An ssh option as a setting of its own is refused, not ignored.
        assert!(parse("[a]\ndestination = \"h\"\nforwards = [\"socks 1\"]\nproxycommand = \"touch /tmp/x\"\n").is_err());
        assert!(parse("[a]\ndestination = \"h\"\nforwards = [\"socks 1\"]\nProxyCommand = \"sh\"\n").is_err());
        // A destination that ssh would read as an option, a URI, or two words.
        for d in ["-oProxyCommand=touch /tmp/x", "-p", "ssh://h", "a b", "h;id", "$(id)", "h\nx", "@h", "u@", "a@b@c", ""] {
            assert!(parse(&format!("[a]\ndestination = {d:?}\nforwards = [\"socks 1\"]\n")).is_err(), "{d:?}");
        }
        // A forward's host, likewise.
        for f in ["local 1 -oProxyCommand=x:2", "local 1 a b:2", "local 1 [x]:2", "local 1 :2", "remote 1 h;id:2", "socks 1 2", "socks 0", "socks 70000", "local 1 h:0", "tunnel 1", "socks -1"] {
            assert!(parse(&format!("[a]\ndestination = \"h\"\nforwards = [{f:?}]\n")).is_err(), "{f:?}");
        }
        // And names: a systemd instance that cannot be anything else.
        for n in ["-a", "A", "a.b", "a@b", "a/b", "\"\"", &"a".repeat(33)] {
            assert!(parse(&format!("[{n}]\ndestination = \"h\"\nforwards = [\"socks 1\"]\n")).is_err(), "{n:?}");
        }
    }

    #[test]
    fn a_wrong_or_empty_tunnel_refuses_the_file() {
        assert!(parse("[a]\ndestination = \"h\"\nforwards = []\n").is_err());
        assert!(parse("[a]\ndestination = \"h\"\n").is_err());
        assert!(parse("[a]\ndestination = \"h\"\nport = 0\nforwards = [\"socks 1\"]\n").is_err());
        let both = parse("[a]\ndestination = \"h\"\nforwards = [\"socks 1080\"]\n[b]\ndestination = \"h\"\nforwards = [\"local 1080 x:1\"]\n");
        assert!(both.unwrap_err().to_string().contains("both listen on 127.0.0.1:1080"));
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(parse(TEMPLATE).unwrap(), vec![]);
    }

    #[test]
    fn one_wrong_tunnel_does_not_stop_the_others() {
        let f = read("[good]\ndestination = \"h\"\nforwards = [\"socks 1080\"]\n[bad]\ndestination = \"-oX\"\nforwards = [\"socks 1\"]\n[odd]\ndestination = \"h\"\nforwards = [\"socks 1\"]\nproxycommand = \"x\"\n[two]\ndestination = \"h\"\nforwards = [\"socks 1080\"]\n").unwrap();
        assert_eq!(f.tunnels.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["good", "two"]);
        let wrong: Vec<&str> = f.wrong.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(wrong, ["bad", "odd"]);
        assert!(f.wrong[1].1.contains("unknown field `proxycommand`"), "{}", f.wrong[1].1);
        assert_eq!(f.clashes, ["tunnels `good` and `two` both listen on 127.0.0.1:1080"]);
        // Not TOML at all: nothing of it can be read.
        assert!(read("[good\n").is_err());
        // A tunnel that is not a table is a wrong tunnel, not a crash.
        assert_eq!(read("x = 1\n").unwrap().wrong.len(), 1);
    }

    #[test]
    fn a_tunnel_added_keeps_the_file_as_written() {
        let text = "# mine\n[a]\ndestination = \"h\"\nforwards = [\"socks 1080\"] # the proxy\n";
        let out = add(text, "b", "me@there", &["local 8080 x:80".into()]).unwrap();
        assert!(out.starts_with(text));
        assert_eq!(parse(&out).unwrap().len(), 2);
        assert!(add(&out, "b", "h", &["socks 1".into()]).unwrap_err().to_string().contains("already"));
        assert!(add(text, "c", "h", &["socks 1080".into()]).is_err(), "a port already used");
        assert!(add(text, "c", "-oProxyCommand=x", &["socks 1".into()]).is_err());
        // A forward that would close the string and add a setting is a forward that does not parse.
        assert!(add(text, "c", "h", &["socks 1\"]\nproxycommand = \"x".into()]).is_err());
        assert_eq!(parse(&add(TEMPLATE, "v", "h", &["socks 1".into()]).unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn a_state_from_the_unit_and_its_last_words() {
        let t = one("[a]\ndestination = \"me@far\"\nforwards = [\"socks 1\"]\n");
        assert_eq!(state("active", "running", true, None, &t), State::Up);
        assert_eq!(state("active", "running", false, None, &t), State::Connecting);
        assert_eq!(state("inactive", "dead", false, None, &t), State::Off);
        assert_eq!(state("", "", false, None, &t), State::Off);
        let r = state("activating", "auto-restart", false, Some("Host key verification failed."), &t);
        assert_eq!(r, State::Retrying("The key of far is not known. Run `ssh me@far` once in a terminal and check it.".into()));
        assert!(r.on() && State::Connecting.on() && !State::Off.on());
        assert!(matches!(state("failed", "failed", false, Some("x"), &t), State::Failed(_)));
        assert_eq!(explain("ssh: connect to host far port 22: Connection timed out", &t), "Cannot reach far. Retrying.");
        assert_eq!(explain("me@far: Permission denied (publickey).", &t), "far refused your key. Is it in your keyring (`ssh-add -l`)?");
        assert_eq!(explain("bind [127.0.0.1]:1080: Address already in use", &t), "A port it listens on is in use. Retrying.");
    }
}
