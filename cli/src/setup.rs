//! First-boot setup (docs/INSTALL.md): on the TorqOS image, before the machine has a configuration,
//! the console asks for one — the machine's name and time zone, the owner's account and password,
//! how to log in remotely — writes the config repo from TorqOS's template, the owner's password
//! outside it, and hands the repo to the broker (`torq-broker setup`: built, switched to, ledgered
//! as change #1). It runs as root, on the console, only while there is no config repo
//! (modules/setup.nix).

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// What the image keeps for setup (modules/setup.nix): its own TorqOS source, the lock pinning it,
/// its disk's hardware file.
const IMAGE: &str = "/etc/torq/image";
/// Nix's caches for the broker's builds (its HOME is /root: modules/broker.nix).
const BUILD_CACHE: &str = "/root/.cache/nix";
/// The broker's history: set up means it has its first entry (change #1).
const LEDGER: &str = "/var/lib/torq/state/ledger.jsonl";
const REPO: &str = "/var/lib/torq/config";
const PASSWORDS: &str = "/var/lib/torq/secrets/passwords";
const BROKER: &str = "/run/current-system/sw/bin/torq-broker";
const SYSTEMD_RUN: &str = "/run/current-system/sw/bin/systemd-run";
const JOURNALCTL: &str = "/run/current-system/sw/bin/journalctl";
/// The broker's own service while it sets the machine up (its log: `journalctl -u torq-setup-broker`).
const BROKER_UNIT: &str = "torq-setup-broker";
const WELCOME: &str = "Welcome to TorqOS. This machine has no configuration yet: a few questions set it up.";
/// TorqOS's wordmark, shown over the welcome (modules/branding.nix).
pub(crate) const BANNER: &str = "/etc/torq/banner";

/// How the owner logs in from other machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remote {
    No,
    Password,
    Key,
    Both,
}

/// What setup asked.
pub struct Answers {
    pub host: String,
    pub zone: String,
    pub user: String,
    /// The owner's password, hashed (SHA-512 crypt, as NixOS reads it): the password itself is never kept.
    pub hash: String,
    pub remote: Remote,
    pub key: Option<String>,
}

/// A host name: one label of letters, digits and hyphens, at most 63 (RFC 1123; modules/host.nix).
pub fn valid_host(h: &str) -> Result<()> {
    let ok = !h.is_empty()
        && h.len() <= 63
        && h.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !h.starts_with('-')
        && !h.ends_with('-');
    if !ok {
        bail!("'{h}': one word of lowercase letters, digits and hyphens (at most 63), like torq or web-01");
    }
    Ok(())
}

/// A time zone this machine has (`zoneinfo`: its tzdata), like Europe/Berlin or UTC.
pub fn valid_zone(z: &str, zoneinfo: &Path) -> Result<()> {
    let plain = !z.is_empty() && !z.contains("..") && !z.starts_with('/') && z.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c));
    if !plain || !zoneinfo.join(z).is_file() {
        bail!("'{z}' is not a time zone here: like Europe/Berlin, America/New_York or UTC");
    }
    Ok(())
}

/// The owner's login name: a lowercase name the broker takes as a person's (pkg::valid_user), not one
/// of the system's own accounts.
pub fn valid_user(u: &str) -> Result<()> {
    const SYSTEM: &[&str] = &["root", "nobody", "daemon", "bin", "sshd", "messagebus", "nixbld", "systemd-network", "systemd-resolve"];
    if !crate::pkg::valid_user(u) || u.len() > 32 || SYSTEM.contains(&u) {
        bail!("'{u}': a login name of lowercase letters, digits, _ and - (starting with a letter), not a system account");
    }
    Ok(())
}

/// One SSH public key line (`ssh-ed25519 AAAA… comment`), as it is pasted: one line of a type
/// TorqOS admits, and a key as OpenSSH reads it (pkg::key_id) — a malformed one would leave a
/// keys-only machine with nobody able to log in. The line returned trimmed.
pub fn valid_key(k: &str) -> Result<String> {
    const TYPES: &[&str] = &[
        "ssh-ed25519",
        "ssh-rsa",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let k = k.trim();
    let mut parts = k.split_whitespace();
    let (kind, body) = (parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
    let base64 = body.len() >= 16 && body.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
    if !TYPES.contains(&kind) || !base64 || k.contains(['\n', '\r']) || k.chars().any(char::is_control) || crate::pkg::key_id(k)?.is_none() {
        bail!("not an SSH public key: one line like `ssh-ed25519 AAAAC3Nza… you@laptop` (the .pub file's content)");
    }
    Ok(k.to_string())
}

/// This machine's hardware file: the one the installer detected (`detected`, ADR-031), when TorqOS
/// was installed from the ISO; otherwise the image's own, a virtual machine's (`image`).
pub fn hardware_file(image: &Path, detected: &Path) -> PathBuf {
    if detected.is_file() { detected.to_path_buf() } else { image.join("hardware-configuration.nix") }
}

/// The config repo's files for `a`: TorqOS's template (`template`: its templates/config) with the
/// answers, the image's lock (`image`), this machine's hardware file (`hardware`: `hardware_file`),
/// TorqOS from `source` (a store path), for
/// `system` ("aarch64-linux"). `flake.nix` comes last: setup runs only while it is missing.
pub fn files(a: &Answers, template: &Path, image: &Path, hardware: &Path, source: &str, system: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let read = |p: PathBuf| std::fs::read(&p).with_context(|| format!("reading {}", p.display()));
    let flake = String::from_utf8(read(template.join("flake.nix"))?).context("flake.nix: not text")?;
    let platform = "system = \"aarch64-linux\";";
    if flake.matches(platform).count() != 1 {
        bail!("TorqOS's template flake.nix does not name its platform once (`{platform}`)");
    }
    let flake = crate::software::with_source(&flake.replace(platform, &format!("system = \"{system}\";")), &format!("path:{source}"))?;
    let torq = json!({ "user": a.user });
    let pretty = |v: serde_json::Value| format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()).into_bytes();
    let mut out: Vec<(String, Vec<u8>)> = vec![
        ("flake.lock".into(), read(image.join("flake.lock"))?),
        ("config-files.json".into(), read(template.join("config-files.json"))?),
        ("hardware-configuration.nix".into(), read(hardware.to_path_buf())?),
        ("nixos/boot.json".into(), read(template.join("nixos/boot.json"))?),
        ("nixos/torq.json".into(), pretty(torq)),
        ("system/host.json".into(), pretty(json!({ "host-name": a.host, "time-zone": a.zone }))),
    ];
    // Remote login: NixOS's own SSH (TorqOS's protections apply: modules/services.nix), with
    // passwords allowed if that was the answer.
    if a.remote != Remote::No {
        let passwords = matches!(a.remote, Remote::Password | Remote::Both);
        let mut openssh = json!({ "enable": true });
        if passwords {
            openssh["settings"] = json!({ "PasswordAuthentication": true });
        }
        out.push(("nixos/services.json".into(), pretty(json!({ "openssh": openssh }))));
    }
    if let Some(key) = &a.key {
        // A public key is not a secret: the configuration declares whose keys log in.
        out.push(("nixos/users.json".into(), pretty(json!({ "users": { a.user.clone(): { "openssh": { "authorizedKeys": { "keys": [key] } } } } }))));
    }
    out.push(("flake.nix".into(), flake.into_bytes()));
    Ok(out)
}

/// The console's setup: ask, write, hand to the broker; again from the start if anything fails.
pub fn run() -> Result<()> {
    if !rustix::process::geteuid().is_root() {
        bail!("setup is the image's first boot, run as root on the console");
    }
    // The image only: what is in the config repo then is a setup's own, cut short, and starting
    // clean removes it. Anywhere else it is a machine's configuration.
    if !Path::new(IMAGE).join("source").exists() {
        bail!("setup is the TorqOS image's, before its first configuration: this machine is not the image");
    }
    // Set up means the history has its first entry: a setup cut short before that (its files
    // written, its build begun) starts again.
    if std::fs::metadata(LEDGER).is_ok_and(|m| m.len() > 0) {
        bail!("this machine is already set up (its history: {LEDGER})");
    }
    let repo = Path::new(REPO);
    loop {
        // Anything a setup cut short left: gone, to start clean. That includes Nix's caches of the
        // broker's build (HOME=/root): Nix writes them without waiting for the disk, so a power cut
        // mid-build can leave them damaged ("database disk image is malformed"), and every later
        // build would fail on them. Before change #1 nothing else on the image uses them.
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(BUILD_CACHE);
        println!("\n{}\n", welcome());
        let a = ask()?;
        match apply(&a) {
            Ok(()) => {
                println!("\nTorqOS is set up (change #1 in its history). Log in as {}.\n", a.user);
                // The console's login prompt, now that there is someone to log in.
                let _ = std::process::Command::new("/run/current-system/sw/bin/systemctl").args(["start", "--no-block", "getty@tty1.service"]).status();
                return Ok(());
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(repo);
                let _ = std::fs::remove_file(Path::new(PASSWORDS).join(&a.user));
                println!("\nSetup did not finish: {e:#}\nNothing was kept; starting over.");
            }
        }
    }
}

fn ask() -> Result<Answers> {
    let host = until(|| line("Machine name", "torq"), |h| valid_host(h).map(|()| h.to_string()))?;
    let zone = until(|| line("Time zone", "UTC"), |z| valid_zone(z, Path::new("/etc/zoneinfo")).map(|()| z.to_string()))?;
    let user = until(|| line("Your user name", ""), |u| valid_user(u).map(|()| u.to_string()))?;
    let hash = loop {
        let p = secret("Password")?;
        if p.chars().count() < 8 {
            println!("  At least 8 characters, please.");
            continue;
        }
        if secret("Password again")? != p {
            println!("  They differ; once more.");
            continue;
        }
        break hash(&p)?;
    };
    println!("\nLogging in from other machines (SSH):\n  1  not at all\n  2  with your password\n  3  with an SSH key\n  4  with either");
    let remote = until(
        // Not at all unless asked for (SECURITY-MODEL: keys only unless passwords are set; audit of
        // 2026-11-01, N7): Enter turns nothing on.
        || line("Choose", "1"),
        |c| match c {
            "1" => Ok(Remote::No),
            "2" => Ok(Remote::Password),
            "3" => Ok(Remote::Key),
            "4" => Ok(Remote::Both),
            _ => bail!("1, 2, 3 or 4"),
        },
    )?;
    let key = if matches!(remote, Remote::Key | Remote::Both) { Some(until(|| line("Your SSH public key (paste one line)", ""), valid_key)?) } else { None };
    let a = Answers { host, zone, user, hash, remote, key };
    let how = match a.remote {
        Remote::No => "not from other machines",
        Remote::Password => "SSH with your password",
        Remote::Key => "SSH with your key",
        Remote::Both => "SSH with your password or key",
    };
    println!("\n  machine {}, time zone {}, user {}, logging in: {how}", a.host, a.zone, a.user);
    if line("Set up the machine with these? (yes / no)", "yes")? != "yes" {
        return ask();
    }
    Ok(a)
}

/// Write the configuration and the password, and hand them to the broker.
fn apply(a: &Answers) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let image = Path::new(IMAGE);
    let source = std::fs::canonicalize(image.join("source")).context("the image's own TorqOS")?;
    let system = match std::env::consts::ARCH {
        "aarch64" => "aarch64-linux",
        "x86_64" => "x86_64-linux",
        other => bail!("TorqOS is not built for {other}"),
    };
    let hardware = hardware_file(image, &Path::new("/").join(crate::install::DETECTED));
    let files = files(a, &source.join("templates/config"), image, &hardware, &source.to_string_lossy(), system)?;
    // The password first, outside the repo, readable by the system only: a new file (whatever was
    // there before, a link included, is gone first).
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(PASSWORDS)?;
    let file = Path::new(PASSWORDS).join(&a.user);
    let _ = std::fs::remove_file(&file);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?;
    writeln!(f, "{}", a.hash)?;
    for (rel, bytes) in &files {
        let p = Path::new(REPO).join(rel);
        std::fs::DirBuilder::new().recursive(true).mode(0o755).create(p.parent().unwrap_or(Path::new(REPO)))?;
        std::fs::write(&p, bytes).with_context(|| format!("writing {}", p.display()))?;
    }
    // The broker as a service of its own, not a child of this console program: the switch starts the
    // console's login prompt, which takes the console from setup (and stops this unit) — the broker
    // finishes regardless, and records change #1.
    println!();
    let ok = {
        let _p = crate::run::Progress::start("Building this machine's system and switching to it");
        std::process::Command::new(SYSTEMD_RUN)
            .args(["--wait", "--collect", "--quiet", &format!("--unit={BROKER_UNIT}"), "--property=StandardOutput=journal", "--property=StandardError=journal", BROKER, "setup"])
            .status()
            .context("running the broker")?
            .success()
    };
    if !ok {
        let log = std::process::Command::new(JOURNALCTL).args(["-u", BROKER_UNIT, "-o", "cat", "-n", "12", "--no-pager"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        bail!("the new configuration did not build:\n{}", log.trim_end());
    }
    Ok(())
}

/// Ask until `check` takes the answer, saying why it did not.
pub(crate) fn until<T>(mut ask: impl FnMut() -> Result<String>, check: impl Fn(&str) -> Result<T>) -> Result<T> {
    loop {
        match check(&ask()?) {
            Ok(v) => return Ok(v),
            Err(e) => println!("  {e:#}"),
        }
    }
}

/// One answer, `default` if none is typed.
fn line(question: &str, default: &str) -> Result<String> {
    line_under(&welcome, question, default)
}

/// One answer under `heading` (shown again if the console is taken over: `asked`), `default` if
/// none is typed.
pub(crate) fn line_under(heading: &dyn Fn() -> String, question: &str, default: &str) -> Result<String> {
    asked(heading, &if default.is_empty() { format!("{question}: ") } else { format!("{question} [{default}]: ") })?;
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("the console closed");
    }
    let s = s.trim();
    Ok(if s.is_empty() { default.to_string() } else { s.to_string() })
}

/// A secret, not shown as it is typed.
fn secret(question: &str) -> Result<String> {
    let _hidden = crate::shell::Echo::set(false);
    asked(&welcome, &format!("{question}: "))?;
    let mut s = String::new();
    if std::io::stdin().lock().read_line(&mut s)? == 0 {
        bail!("the console closed");
    }
    println!();
    Ok(s.trim_end_matches(['\n', '\r']).to_string())
}

/// The welcome, under TorqOS's wordmark when the system has one.
fn welcome() -> String {
    match std::fs::read_to_string(BANNER) {
        Ok(banner) => format!("{}\n\n{WELCOME}", banner.trim_end()),
        Err(_) => WELCOME.to_string(),
    }
}

/// Ask, and wait until an answer is typed. A graphics driver loading late (bochs-drm on QEMU's
/// standard VGA, seconds into setup; a GPU's on real hardware) takes the console over, and what was on
/// the screen may be lost — the question with it, leaving a screen that asks nothing. Such a console
/// changes size (VGA text's 80x25 to the framebuffer's), so while waiting a size change means the
/// question is shown again, under `heading`. (Not timed to the driver: nothing tells whether one is
/// coming, or when.) A console hands over one line a read, so nothing typed sits unread in stdin's
/// buffer while this waits on the terminal itself.
fn asked(heading: &dyn Fn() -> String, prompt: &str) -> Result<()> {
    use rustix::event::{poll, PollFd, PollFlags, Timespec};
    let stdin = std::io::stdin();
    let size = || rustix::termios::tcgetwinsize(&stdin).ok().map(|w| (w.ws_row, w.ws_col));
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut was = size();
    loop {
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        if poll(&mut fds, Some(&Timespec { tv_sec: 0, tv_nsec: 500_000_000 }))? > 0 {
            return Ok(());
        }
        let now = size();
        if now != was {
            was = now;
            print!("\n{}\n\n{prompt}", heading());
            std::io::stdout().flush()?;
        }
    }
}

/// A password as NixOS reads it: SHA-512 crypt, with many rounds (slow to guess, quick to check
/// once). `request system password` uses the same.
pub fn hash(password: &str) -> Result<String> {
    let params = sha_crypt::Sha512Params::new(200_000).map_err(|e| anyhow::anyhow!("hashing the password: {e:?}"))?;
    sha_crypt::sha512_simple(password, &params).map_err(|e| anyhow::anyhow!("hashing the password: {e:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answers(remote: Remote, key: Option<&str>) -> Answers {
        Answers { host: "web-01".into(), zone: "UTC".into(), user: "alice".into(), hash: "$6$x$y".into(), remote, key: key.map(String::from) }
    }

    #[test]
    fn answers_are_checked() {
        valid_host("web-01").unwrap();
        for bad in ["", "-a", "a-", "Web", "a.b", "a b", &"a".repeat(64)] {
            assert!(valid_host(bad).is_err(), "{bad:?}");
        }
        let zones = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(zones.path().join("Europe")).unwrap();
        std::fs::write(zones.path().join("Europe/Berlin"), "").unwrap();
        valid_zone("Europe/Berlin", zones.path()).unwrap();
        for bad in ["Europe/Nowhere", "../etc/passwd", "/etc/passwd", "Europe", ""] {
            assert!(valid_zone(bad, zones.path()).is_err(), "{bad:?}");
        }
        valid_user("alice").unwrap();
        for bad in ["root", "Alice", "1x", "a b", "nobody", ""] {
            assert!(valid_user(bad).is_err(), "{bad:?}");
        }
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBvgBOByqNMQxVNX1cIbQG/J5i0lGrLOjoNzyapKOo5G alice@laptop";
        assert_eq!(valid_key(&format!("  {key}\n")).unwrap(), key);
        // Shaped like a key, but not one (the audit's): OpenSSH says so.
        for bad in ["ssh-ed25519 AAAAAAAAAAAAAAAA", "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGtorq alice@laptop", "ssh-dss AAAAB3NzaC1kc3MAAACBAx", "hello", "ssh-ed25519", "ssh-ed25519 not*base64*at-all!"] {
            assert!(valid_key(bad).is_err(), "{bad:?}");
        }
    }

    /// The files are TorqOS's template with the answers: TorqOS from the image's store path, the
    /// machine's platform, SSH as chosen, the password nowhere in them, flake.nix last.
    #[test]
    fn the_config_repo_from_the_template() {
        // Read where the repo is (the dev shell); the package's own build has only cli/, and skips it.
        let template = Path::new(env!("CARGO_MANIFEST_DIR")).join("../templates/config");
        if !template.join("flake.nix").exists() {
            return;
        }
        let image = tempfile::tempdir().unwrap();
        std::fs::write(image.path().join("flake.lock"), "{\"lock\":1}\n").unwrap();
        std::fs::write(image.path().join("hardware-configuration.nix"), "{ ... }: { }\n").unwrap();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGtorq alice@laptop";
        let src = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-source";
        let f = files(&answers(Remote::Both, Some(key)), &template, image.path(), &image.path().join("hardware-configuration.nix"), src, "x86_64-linux").unwrap();
        let get = |name: &str| String::from_utf8(f.iter().find(|(p, _)| p == name).unwrap_or_else(|| panic!("{name}")).1.clone()).unwrap();
        assert_eq!(f.last().unwrap().0, "flake.nix", "written last: setup runs while it is missing");
        let flake = get("flake.nix");
        assert!(flake.contains(&format!("torq.url = \"path:{src}\";")) && flake.contains("system = \"x86_64-linux\";"), "{flake}");
        assert_eq!(crate::software::source_of(&flake), Some(format!("path:{src}")));
        assert!(!crate::software::writable_folder(&format!("path:{src}")), "a store path: any machine's");
        assert!(hash("correct horse").unwrap().starts_with("$6$rounds=200000$"), "slow to guess");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&get("nixos/torq.json")).unwrap(), json!({ "user": "alice" }));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&get("system/host.json")).unwrap(), json!({ "host-name": "web-01", "time-zone": "UTC" }));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&get("nixos/services.json")).unwrap(), json!({ "openssh": { "enable": true, "settings": { "PasswordAuthentication": true } } }));
        assert!(get("nixos/users.json").contains(key));
        assert_eq!(get("flake.lock"), "{\"lock\":1}\n");
        assert!(f.iter().all(|(_, b)| !String::from_utf8_lossy(b).contains("$6$")), "the password is never in the repo");
        // No remote login: no SSH at all; a key only: no passwords over SSH.
        let none = files(&answers(Remote::No, None), &template, image.path(), &image.path().join("hardware-configuration.nix"), "/nix/store/abc-source", "aarch64-linux").unwrap();
        assert!(none.iter().all(|(p, _)| p != "nixos/services.json" && p != "nixos/users.json"));
        let keyed = files(&answers(Remote::Key, Some(key)), &template, image.path(), &image.path().join("hardware-configuration.nix"), "/nix/store/abc-source", "aarch64-linux").unwrap();
        let services = &keyed.iter().find(|(p, _)| p == "nixos/services.json").unwrap().1;
        assert_eq!(serde_json::from_slice::<serde_json::Value>(services).unwrap(), json!({ "openssh": { "enable": true } }), "keys only: NixOS's SSH, as TorqOS sets it up");
    }

    /// Installed from the ISO, the machine's own hardware (what the installer detected) is the one
    /// in its config repo; installed from the image, the image's (ADR-031).
    #[test]
    fn the_detected_hardware_over_the_images() {
        let image = tempfile::tempdir().unwrap();
        std::fs::write(image.path().join("hardware-configuration.nix"), "# the image's\n").unwrap();
        let detected = image.path().join("install/hardware-configuration.nix");
        assert_eq!(hardware_file(image.path(), &detected), image.path().join("hardware-configuration.nix"), "none detected: the image's");
        std::fs::create_dir_all(detected.parent().unwrap()).unwrap();
        std::fs::write(&detected, "# detected\n").unwrap();
        assert_eq!(hardware_file(image.path(), &detected), detected, "detected: that one");
        std::fs::remove_file(&detected).unwrap();
        std::fs::create_dir(&detected).unwrap();
        assert_eq!(hardware_file(image.path(), &detected), image.path().join("hardware-configuration.nix"), "only a file counts");
    }
}
