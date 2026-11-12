//! SSH with a secret the program never holds (ADR-052, step 3). A program asks for an agent socket
//! (`POST /ssh-agent`, as an HTTPS use is asked: the broker reads who from the kernel and grants it); the daemon
//! opens one only that account can connect to, and speaks OpenSSH's agent protocol on it. It lists the keys the
//! account may use (their public halves) and passes on nothing but a signature, made like this: the daemon starts
//! a private `ssh-agent` of its own, loads the key into it restricted to the rule's host (`ssh-add -h`, with that
//! host's keys from the rule), replays the session binding the program's ssh sent, and asks it to sign. OpenSSH's
//! agent checks that the session is with that host, by its key, and refuses to sign for any other: that check is
//! OpenSSH's own, not the daemon's. The private agent is gone after the one signature.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use torq_wire::secrets::{base64_decode, ssh_string as string};

/// Where the agent sockets are (the unit's runtime directory).
pub const DIR: &str = "/run/torq-secrets";
/// The largest agent message taken.
const MESSAGE_MAX: usize = 256 * 1024;

// The agent protocol's numbers (draft-miller-ssh-agent).
const FAILURE: u8 = 5;
const SUCCESS: u8 = 6;
const REQUEST_IDENTITIES: u8 = 11;
const IDENTITIES_ANSWER: u8 = 12;
const SIGN_REQUEST: u8 = 13;
const EXTENSION: u8 = 27;
const EXTENSION_FAILURE: u8 = 28;

/// A key the socket's account may use, as the broker listed it.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Key {
    pub owner: serde_json::Value,
    pub name: String,
    pub public: String,
    pub known: Vec<String>,
}

impl Key {
    /// Its owner as a route names one: None for the system's.
    fn user(&self) -> Option<String> {
        self.owner.get("person").and_then(|p| p.as_str()).map(String::from)
    }

    fn blob(&self) -> Option<Vec<u8>> {
        base64_decode(self.public.split(' ').nth(1)?)
    }
}

/// What the broker granted for a socket.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Granted {
    pub grant: String,
    pub actor: String,
    pub uid: u32,
    pub keys: Vec<Key>,
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

/// One message from `r`: its type and body (None: the connection ended).
fn read_message(r: &mut impl Read) -> Result<Option<(u8, Vec<u8>)>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n == 0 || n > MESSAGE_MAX {
        bail!("an agent message of {n} bytes");
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    Ok(Some((body[0], body[1..].to_vec())))
}

fn message(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(body);
    out
}

/// The answer to a list of identities: the keys the account may use.
pub fn identities(keys: &[Key]) -> Vec<u8> {
    let usable: Vec<(Vec<u8>, String)> = keys.iter().filter_map(|k| Some((k.blob()?, format!("torq:{}/{}", k.user().unwrap_or_else(|| "system".into()), k.name)))).collect();
    let mut body = (usable.len() as u32).to_be_bytes().to_vec();
    for (blob, comment) in usable {
        put_string(&mut body, &blob);
        put_string(&mut body, comment.as_bytes());
    }
    message(IDENTITIES_ANSWER, &body)
}

/// The host a session binding is with, by its key, among `known` (the rule's known_hosts lines): its name.
pub fn bound_host(known: &[String], hostkey: &[u8]) -> Option<String> {
    known.iter().find_map(|l| {
        let f: Vec<&str> = l.split(' ').collect();
        (f.len() == 3 && base64_decode(f[2]).as_deref() == Some(hostkey)).then(|| f[0].to_string())
    })
}

/// What a program's agent asks a key to sign for, as the daemon is told: the function that asks the broker.
pub type Ask = fn(&Key, &str, &str) -> Result<std::result::Result<(String, Vec<String>), String>>;

/// The most agent sockets one account has open at once, and connections to one socket at once.
const SOCKETS_EACH: usize = 4;
const CONNECTIONS_EACH: usize = 4;
static SOCKETS: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// A socket for `g`: made, served until the grant ends, then gone. Its path.
pub fn open(g: Granted, ask: Ask) -> Result<PathBuf> {
    {
        let mut open = SOCKETS.lock().map_err(|_| anyhow::anyhow!("the daemon's count of sockets"))?;
        if open.iter().filter(|u| **u == g.uid).count() >= SOCKETS_EACH {
            bail!("{} has {SOCKETS_EACH} agent sockets open already: use one of them", g.actor);
        }
        open.push(g.uid);
    }
    let release = |uid: u32| {
        if let Ok(mut open) = SOCKETS.lock() {
            if let Some(i) = open.iter().position(|u| *u == uid) {
                open.remove(i);
            }
        }
    };
    let made = (|| -> Result<(PathBuf, UnixListener)> {
        let mut bytes = [0u8; 12];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let path = Path::new(DIR).join(format!("ssh-{}.sock", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        let listener = UnixListener::bind(&path).with_context(|| format!("making {}", path.display()))?;
        // Anyone may connect; only the account it was granted to is answered (SO_PEERCRED, below).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))?;
        listener.set_nonblocking(true)?;
        Ok((path, listener))
    })();
    let (path, listener) = match made {
        Ok(m) => m,
        Err(e) => {
            release(g.uid);
            return Err(e);
        }
    };
    let served = path.clone();
    let uid = g.uid;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3600);
    let spawned = std::thread::Builder::new().name("agent-socket".into()).spawn(move || {
        let live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        while std::time::Instant::now() < until {
            match listener.accept() {
                Ok((s, _)) => {
                    use std::sync::atomic::Ordering;
                    if live.fetch_add(1, Ordering::SeqCst) >= CONNECTIONS_EACH {
                        live.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let (g, live2) = (g.clone(), live.clone());
                    let one = std::thread::Builder::new().name("agent-connection".into()).spawn(move || {
                        let _ = s.set_nonblocking(false);
                        let _ = serve(s, &g, ask);
                        live2.fetch_sub(1, Ordering::SeqCst);
                    });
                    if one.is_err() {
                        live.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(_) => break,
            }
        }
        let _ = std::fs::remove_file(&served);
        release(uid);
    });
    if let Err(e) = spawned {
        let _ = std::fs::remove_file(&path);
        release(uid);
        bail!("no thread for the socket: {e}");
    }
    Ok(path)
}

/// A session's binding, as ssh sent it (OpenSSH's session-bind@openssh.com): the message, the host's key, and the
/// session's identifier.
struct Bind {
    message: Vec<u8>,
    hostkey: Vec<u8>,
    session: Vec<u8>,
}

/// A binding from an extension's body (after its name), if it is one this daemon takes: a direct session only. A
/// binding marked as forwarded (a hop through another host, `ssh -A`) is refused, as is a second binding on one
/// connection: the relay replays one binding, so a chain of hops is not one it could have OpenSSH check.
fn bind_of(body: &[u8], rest: &[u8]) -> std::result::Result<Bind, &'static str> {
    let (hostkey, rest) = string(rest).ok_or("a binding without a host key")?;
    let (session, rest) = string(rest).ok_or("a binding without a session")?;
    let (_signature, rest) = string(rest).ok_or("a binding without a signature")?;
    match rest.first() {
        Some(0) => {}
        Some(_) => return Err("a forwarded session (a hop through another host): not taken"),
        None => return Err("a binding without its forwarding flag"),
    }
    Ok(Bind { message: message(EXTENSION, body), hostkey: hostkey.to_vec(), session: session.to_vec() })
}

/// One connection to a socket: the account it was granted to, and only that one, speaking the agent protocol.
fn serve(mut s: UnixStream, g: &Granted, ask: Ask) -> Result<()> {
    let peer = rustix::net::sockopt::socket_peercred(&s).context("who is connected")?;
    if peer.uid.as_raw() != g.uid {
        crate::log(&format!("refused an agent connection from uid {}: the socket is {}'s", peer.uid.as_raw(), g.actor));
        return Ok(());
    }
    s.set_read_timeout(Some(std::time::Duration::from_secs(300)))?;
    let mut bind: Option<Bind> = None;
    while let Some((kind, body)) = read_message(&mut s)? {
        let answer = match kind {
            REQUEST_IDENTITIES => identities(&g.keys),
            EXTENSION => match string(&body) {
                Some((name, rest)) if name == b"session-bind@openssh.com" => match (bind.is_some(), bind_of(&body, rest)) {
                    (false, Ok(b)) => {
                        // Not yet checked: OpenSSH's own agent checks it (the host's signature of the session) before
                        // anything is asked of the broker, at the signature.
                        bind = Some(b);
                        message(SUCCESS, &[])
                    }
                    (true, _) => {
                        crate::log(&format!("{}: a second session binding on one connection: refused", g.actor));
                        message(FAILURE, &[])
                    }
                    (false, Err(why)) => {
                        crate::log(&format!("{}: {why}", g.actor));
                        message(FAILURE, &[])
                    }
                },
                _ => message(EXTENSION_FAILURE, &[]),
            },
            SIGN_REQUEST => match sign(g, &body, bind.as_ref(), ask) {
                Ok(a) => a,
                Err(e) => {
                    crate::log(&format!("{}: no signature: {e:#}", g.actor));
                    message(FAILURE, &[])
                }
            },
            // Adding, removing, locking: never.
            _ => message(FAILURE, &[]),
        };
        s.write_all(&answer)?;
    }
    Ok(())
}

/// A signature, by a private ssh-agent of the daemon's that holds the key for this one request, restricted to the
/// rule's host. In this order: OpenSSH's agent checks the session's binding (the host's own signature of it) while
/// it holds no key; the request must be to log in to that session; only then is the broker asked (which records the
/// agent's first use and gives the value), and the key added, restricted to the host, and the request signed.
fn sign(g: &Granted, body: &[u8], bind: Option<&Bind>, ask: Ask) -> Result<Vec<u8>> {
    let (blob, rest) = string(body).context("a sign request without a key")?;
    let (data, _flags) = string(rest).context("a sign request without its data")?;
    let key = g.keys.iter().find(|k| k.blob().as_deref() == Some(blob)).context("not a key this socket may use")?;
    let bind = bind.context("no session binding: ssh (OpenSSH 8.9 or later) binds its session first, and a key here signs only for a bound one")?;
    let host = bound_host(&key.known, &bind.hostkey).context("the session is with a host this key's rule does not name (by its key)")?;
    // A login to the bound session: what is signed starts with that session's identifier.
    if string(data).map(|(s, _)| s) != Some(bind.session.as_slice()) {
        bail!("not a login to the bound session: a key here signs nothing else");
    }
    let dir = TempDir::new()?;
    let sock = dir.0.join("agent.sock");
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut agent = Command::new("ssh-agent").arg("-D").arg("-a").arg(&sock).env_clear().env("PATH", &path).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().context("running ssh-agent")?;
    let result = (|| -> Result<Vec<u8>> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !sock.exists() {
            if std::time::Instant::now() > deadline {
                bail!("ssh-agent did not start");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut a = UnixStream::connect(&sock)?;
        a.set_read_timeout(Some(std::time::Duration::from_secs(30)))?;
        // The binding first, to an agent holding nothing: it takes it only if the host's signature of the session holds.
        a.write_all(&bind.message)?;
        match read_message(&mut a)? {
            Some((SUCCESS, _)) => {}
            _ => bail!("OpenSSH's agent did not take the session's binding: it is not the host's"),
        }
        let (value, known) = match ask(key, &g.grant, &host)? {
            Ok(v) => v,
            Err(why) => bail!("{why}"),
        };
        let known_file = dir.0.join("known_hosts");
        std::fs::write(&known_file, known.iter().map(|l| format!("{l}\n")).collect::<String>())?;
        // The key, restricted to the host (`-h`), whose keys are the rule's (`-H`); its value on ssh-add's input.
        let mut add = Command::new("ssh-add")
            .args(["-q", "-h", &host, "-H"])
            .arg(&known_file)
            .arg("-")
            .env_clear()
            .env("PATH", &path)
            .env("SSH_AUTH_SOCK", &sock)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("running ssh-add")?;
        add.stdin.take().context("ssh-add's input")?.write_all(format!("{}\n", value.trim_end()).as_bytes())?;
        drop(value);
        let added = add.wait_with_output()?;
        if !added.status.success() {
            bail!("ssh-add refused the key: {}", String::from_utf8_lossy(&added.stderr).chars().filter(|c| !c.is_control()).take(200).collect::<String>());
        }
        a.write_all(&message(SIGN_REQUEST, body))?;
        let (kind, answer) = read_message(&mut a)?.context("OpenSSH's agent hung up")?;
        Ok(message(kind, &answer))
    })();
    let _ = agent.kill();
    let _ = agent.wait();
    let answer = result?;
    crate::log(&format!("{} used {} for ssh to {host}: {}", g.actor, whose(key), if answer.get(4) == Some(&14) { "signed" } else { "refused by OpenSSH's agent" }));
    Ok(answer)
}

fn whose(k: &Key) -> String {
    match k.user() {
        None => format!("the system's secret {}", k.name),
        Some(u) => format!("{u}'s secret {}", k.name),
    }
}

/// A private directory of the daemon's (its unit's private /tmp), gone when this is.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<TempDir> {
        use std::os::unix::fs::DirBuilderExt;
        let mut bytes = [0u8; 8];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let p = std::env::temp_dir().join(format!("torq-secrets-ssh-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        std::fs::DirBuilder::new().mode(0o700).create(&p)?;
        Ok(TempDir(p))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    #[test]
    fn base64_as_keys_carry_it() {
        let b = base64_decode(ED).unwrap();
        assert_eq!(&b[..15], b"\0\0\0\x0bssh-ed25519");
        assert_eq!(base64_decode("aGVsbG8="), Some(b"hello".to_vec()));
        assert_eq!(base64_decode("a"), None);
        assert_eq!(base64_decode("a$bc"), None);
    }

    #[test]
    fn the_host_is_known_by_its_key() {
        let known = vec![format!("github.com ssh-ed25519 {ED}")];
        assert_eq!(bound_host(&known, &base64_decode(ED).unwrap()).as_deref(), Some("github.com"));
        assert_eq!(bound_host(&known, b"another key"), None);
    }

    #[test]
    fn messages_and_identities() {
        let k = Key { owner: serde_json::json!({"person": "sam"}), name: "deploy".into(), public: format!("ssh-ed25519 {ED} sam@x"), known: vec![] };
        let m = identities(&[k]);
        let (kind, body) = read_message(&mut &m[..]).unwrap().unwrap();
        assert_eq!(kind, IDENTITIES_ANSWER);
        assert_eq!(&body[..4], &1u32.to_be_bytes());
        let (blob, rest) = string(&body[4..]).unwrap();
        assert_eq!(blob, base64_decode(ED).unwrap().as_slice());
        assert_eq!(string(rest).unwrap().0, b"torq:sam/deploy");
        assert!(read_message(&mut &[0u8, 0, 0, 0][..]).is_err(), "an empty message");
        assert!(read_message(&mut &((MESSAGE_MAX as u32 + 1).to_be_bytes())[..]).is_err(), "too large");
        assert_eq!(read_message(&mut &[][..]).unwrap(), None);
    }
}
