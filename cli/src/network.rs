//! Setup's network step (ADR-048, amended: network first). On the image's own boot, before any other
//! question: a cable that is up is used as it is; otherwise a Wi-Fi device is brought up — the kernel
//! pointed at the firmware the installer carried, the device probed again — and a network joined with
//! iwd, whose saved networks are this disk's, so the installed system rejoins by itself. Or none: the
//! install is whole without a network (ADR-048 5).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = "/run/current-system/sw/bin";

/// Whether this machine has an address to reach the world from: an interface, not the loopback, with a
/// global IPv4 or IPv6 address (`ip -o addr show scope global` lists one).
pub fn online() -> bool {
    Command::new(format!("{BIN}/ip"))
        .args(["-o", "addr", "show", "scope", "global"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| has_address(&String::from_utf8_lossy(&o.stdout)))
}

/// `ip -o addr show scope global`'s output names an interface with an address.
pub fn has_address(ip: &str) -> bool {
    ip.lines().any(|l| {
        let mut w = l.split_whitespace();
        let (_, dev, fam) = (w.next(), w.next(), w.next());
        dev.is_some_and(|d| d != "lo") && matches!(fam, Some("inet" | "inet6"))
    })
}

/// The wireless interfaces under `sys` (`/sys`): their names.
pub fn wireless(sys: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(sys.join("class/net"))
        .map(|d| d.flatten().filter(|e| e.path().join("wireless").exists() || e.path().join("phy80211").exists()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// The PCI Wi-Fi devices under `sys` (class 0x0280) that have no network interface: their drivers gave up,
/// usually for want of firmware on the image's own boot.
pub fn stalled(sys: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(sys.join("bus/pci/devices"))
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| std::fs::read_to_string(p.join("class")).is_ok_and(|c| c.trim().starts_with("0x0280")))
                .filter(|p| std::fs::read_dir(p.join("net")).map(|mut n| n.next().is_none()).unwrap_or(true))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The firmware folder among the paths a registration names (closureInfo's: a path, then its hash, size,
/// deriver and references, each a line): NixOS's combined `firmware`, whose `lib/firmware` exists.
pub fn firmware_in(registration: &str, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    registration
        .lines()
        .filter(|l| l.starts_with("/nix/store/") && l.ends_with("-firmware") && !l.contains(' '))
        .map(|l| Path::new(l).join("lib/firmware"))
        .find(|p| exists(p))
}

/// Bring up a Wi-Fi device whose driver gave up: point the kernel at the carried firmware (from
/// `registration`, the installer's), remove the device and let the bus find it again. Waits for its
/// interface. Nothing to do, nothing done.
pub fn wake(sys: &Path, registration: &Path) -> Result<()> {
    let stalled = stalled(sys);
    if stalled.is_empty() || !wireless(sys).is_empty() {
        return Ok(());
    }
    let reg = std::fs::read_to_string(registration).context("the installer's list of what it carried (no firmware without it)")?;
    let fw = firmware_in(&reg, |p| p.is_dir()).context("the firmware the installer carried")?;
    std::fs::write(sys.join("module/firmware_class/parameters/path"), fw.to_string_lossy().as_bytes()).context("pointing the kernel at the firmware")?;
    for dev in &stalled {
        std::fs::write(dev.join("remove"), "1").with_context(|| format!("removing {} to probe it again", dev.display()))?;
    }
    std::fs::write(sys.join("bus/pci/rescan"), "1").context("finding the Wi-Fi device again")?;
    let until = Instant::now() + Duration::from_secs(20);
    while wireless(sys).is_empty() {
        if Instant::now() > until {
            bail!("the Wi-Fi device did not come up with its firmware");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

/// A network iwd sees: its name, and how strong (iwctl's stars, 0–4).
#[derive(Debug, PartialEq)]
pub struct Seen {
    pub name: String,
    pub security: String,
    pub signal: usize,
}

/// iwctl's `station <dev> get-networks`, colours removed: the networks it lists, strongest first.
pub fn networks(out: &str) -> Vec<Seen> {
    let plain = strip_ansi(out);
    let mut v: Vec<Seen> = plain
        .lines()
        .filter_map(|l| {
            // Rows are "  [>] name   security   ****"; columns are two or more spaces apart, a name may hold one.
            let l = l.trim_start().trim_start_matches('>').trim();
            let cols: Vec<&str> = l.split("  ").map(str::trim).filter(|c| !c.is_empty()).collect();
            match cols.as_slice() {
                [name, security, signal] if signal.chars().all(|c| c == '*') && !signal.is_empty() => {
                    Some(Seen { name: name.to_string(), security: security.to_string(), signal: signal.len() })
                }
                _ => None,
            }
        })
        .collect();
    v.sort_by(|a, b| b.signal.cmp(&a.signal).then(a.name.cmp(&b.name)));
    v.dedup_by(|a, b| a.name == b.name);
    v
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn iwctl(args: &[&str]) -> Result<String> {
    let o = Command::new(format!("{BIN}/iwctl")).args(args).stdin(Stdio::null()).output().context("running iwctl")?;
    if !o.status.success() {
        bail!("iwctl {}: {}", args.join(" "), strip_ansi(&String::from_utf8_lossy(&o.stdout)).trim());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The network step: online already (a cable), nothing to ask; a Wi-Fi device, a network chosen and
/// joined, or skipped; neither, said and gone on. Whether the machine is online after it.
pub fn step(heading: &dyn Fn() -> String) -> Result<bool> {
    if online() {
        println!("Network: connected.");
        return Ok(true);
    }
    let sys = Path::new("/sys");
    if let Err(e) = wake(sys, &Path::new("/").join(crate::install::REGISTRATION)) {
        println!("Network: Wi-Fi could not be brought up ({e:#}).");
    }
    let Some(dev) = wireless(sys).into_iter().next() else {
        println!("Network: none (no cable, no Wi-Fi). TorqOS installs without one.");
        return Ok(false);
    };
    loop {
        let _ = iwctl(&["station", &dev, "scan"]);
        std::thread::sleep(Duration::from_secs(4));
        let seen = networks(&iwctl(&["station", &dev, "get-networks"]).unwrap_or_default());
        let list = seen
            .iter()
            .enumerate()
            .map(|(i, s)| format!("  {:>2}  {}  ({}, {})", i + 1, s.name, s.security, ["very weak", "weak", "fair", "good", "strong"][s.signal.min(4)]))
            .collect::<Vec<_>>()
            .join("\n");
        let menu = format!("Network: no cable. Wi-Fi networks seen:\n{}\n   r  look again\n   s  skip: continue without a network", if seen.is_empty() { "  (none yet)".to_string() } else { list });
        println!("\n{menu}");
        let shown = || format!("{}\n\n{menu}", heading());
        let pick = crate::setup::line_under(&shown, "Join", if seen.is_empty() { "r" } else { "1" })?;
        match pick.as_str() {
            "s" => return Ok(false),
            "r" => continue,
            n => {
                let Some(s) = n.parse::<usize>().ok().and_then(|i| i.checked_sub(1)).and_then(|i| seen.get(i)) else {
                    println!("  A number from the list, r or s.");
                    continue;
                };
                // iwctl asks the passphrase itself, on this console: it is never in a command line.
                println!("Joining {} ...", s.name);
                let ok = Command::new(format!("{BIN}/iwctl")).args(["station", &dev, "connect", &s.name]).status().is_ok_and(|st| st.success());
                if ok && wait_online(Duration::from_secs(30)) {
                    println!("Network: connected to {}.", s.name);
                    return Ok(true);
                }
                println!("  Not connected to {} (a wrong passphrase, or no address given). Try again, or s to skip.", s.name);
            }
        }
    }
}

fn wait_online(within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        if online() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_online_the_loopback_is_not() {
        assert!(!has_address(""));
        assert!(!has_address("1: lo    inet 127.0.0.1/8 scope host lo\n"));
        assert!(has_address("3: wlan0    inet 192.168.50.38/24 metric 1025 brd 192.168.50.255 scope global dynamic wlan0\n"));
        assert!(has_address("2: eth0    inet6 2001:db8::5/64 scope global\n"));
    }

    /// iwctl's list, in its colours, read as the networks with their signal; strongest first, each once;
    /// a name with a space kept whole; headings and rules not networks.
    #[test]
    fn networks_from_iwctl() {
        let out = "\x1b[1;90m                               Available networks\x1b[0m\n\
            --------------------------------------------------------------------------------\n\
            \x1b[1;90m      Network name                      Security            Signal\x1b[0m\n\
            --------------------------------------------------------------------------------\n\
            \x1b[0m    Neighbor                          psk                 \x1b[1;90m**\x1b[0m\n\
            \x1b[0m  > Home Net 5G                       psk                 ****\n\
            \x1b[0m    Cafe                              open                *\x1b[1;90m***\x1b[0m\n";
        let n = networks(out);
        assert_eq!(n.iter().map(|s| (s.name.as_str(), s.signal)).collect::<Vec<_>>(), [("Cafe", 4), ("Home Net 5G", 4), ("Neighbor", 2)]);
        assert_eq!(n[1].security, "psk");
        assert!(networks("No networks found\n").is_empty());
    }

    #[test]
    fn a_stalled_wifi_device_and_its_firmware() {
        let sys = tempfile::tempdir().unwrap();
        let s = sys.path();
        let dev = |a: &str, class: &str, net: Option<&str>| {
            let p = s.join("bus/pci/devices").join(a);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("class"), format!("{class}\n")).unwrap();
            if let Some(n) = net {
                std::fs::create_dir_all(p.join("net").join(n)).unwrap();
                std::fs::create_dir_all(s.join("class/net").join(n).join("wireless")).unwrap();
            }
        };
        dev("0000:01:00.0", "0x020000", None);
        dev("0000:c3:00.0", "0x028000", None);
        assert_eq!(stalled(s), [s.join("bus/pci/devices/0000:c3:00.0")]);
        assert!(wireless(s).is_empty());
        dev("0000:c4:00.0", "0x028000", Some("wlan0"));
        assert_eq!(stalled(s).len(), 1, "a device with its interface is not stalled");
        assert_eq!(wireless(s), ["wlan0"]);
        let reg = "/nix/store/aaaa-iwd-3.12\nsha256:x\n10\n\n0\n/nix/store/bbbb-firmware\nsha256:y\n20\n\n0\n/nix/store/cccc-linux-firmware-20261019-zstd\n";
        assert_eq!(firmware_in(reg, |p| p.starts_with("/nix/store/bbbb-firmware")), Some(PathBuf::from("/nix/store/bbbb-firmware/lib/firmware")));
        assert_eq!(firmware_in(reg, |_| false), None);
    }
}
