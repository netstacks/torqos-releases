//! What the machine is and how it is doing — `show version`, `show system uptime`, `show
//! interfaces`, `show system storage`, `show users` — as a network device answers them. The facts are read from
//! the running system (shell.rs: /proc, `ip -j`, `df`); laying them out is pure, and tested here.

use crate::ui::{Col, Table, Ui};

/// The running machine, read at the moment of asking.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// NixOS's release (`26.05.20261029.6f06d20`).
    pub nixos: String,
    /// The Linux kernel's release.
    pub kernel: String,
    /// Seconds since the machine started.
    pub uptime: u64,
    /// The load averages over 1, 5 and 15 minutes.
    pub load: [f64; 3],
    pub interfaces: Vec<Interface>,
    pub filesystems: Vec<Filesystem>,
    /// The system versions kept, each one bootable.
    pub generations: usize,
    /// The people with an account here.
    pub people: Vec<Person>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Interface {
    pub name: String,
    /// As the kernel says it: up, down, unknown (a loopback).
    pub state: String,
    pub mac: String,
    /// Addresses with their prefix length (`192.168.64.4/24`).
    pub addresses: Vec<String>,
}

/// A person's account on the machine (`show users`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Person {
    pub name: String,
    /// Their groups: their own first, then the others as /etc/group lists them.
    pub groups: Vec<String>,
    /// Where they are logged in now (`pts/0 from 192.168.64.1`, `tty1`).
    pub sessions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filesystem {
    pub device: String,
    pub mount: String,
    /// In bytes.
    pub size: u64,
    pub used: u64,
    pub free: u64,
}

/// `show version`: what this machine runs. `software`: where TorqOS comes from and when it is from
/// (the configuration's lock, as `show configuration` names it).
pub fn version(f: &Facts, software: &str, ui: &Ui) -> String {
    rows(
        &[
            ("TorqOS", software.to_string()),
            ("NixOS", f.nixos.clone()),
            ("Linux kernel", f.kernel.clone()),
            ("torqcli", env!("CARGO_PKG_VERSION").to_string()),
        ],
        ui,
    )
}

/// `show system uptime`: how long the machine has run, its load, and its last change.
pub fn uptime(f: &Facts, last: Option<&str>, ui: &Ui) -> String {
    let mut r = vec![("Up", duration(f.uptime)), ("Load", format!("{:.2} {:.2} {:.2} (over 1, 5 and 15 minutes)", f.load[0], f.load[1], f.load[2]))];
    r.extend(last.map(|l| ("Last change", l.to_string())));
    rows(&r, ui)
}

/// `show interfaces [<name>]`: each network interface (or the one named), its state, addresses and
/// hardware address. Data: never wrapped (a filter reads whole lines).
pub fn interfaces(f: &Facts, name: Option<&str>) -> anyhow::Result<String> {
    if let Some(n) = name.filter(|n| !f.interfaces.iter().any(|i| i.name == *n)) {
        let names: Vec<&str> = f.interfaces.iter().map(|i| i.name.as_str()).collect();
        anyhow::bail!("'{n}' is not an interface of this machine{} (one of: {})", crate::near::hint(n, names.iter().copied()), names.join(", "));
    }
    let mut t = Table::new(&[Col::Left("Interface"), Col::Left("State"), Col::Left("Address"), Col::Left("Hardware address")]);
    for i in f.interfaces.iter().filter(|i| name.is_none_or(|n| i.name == n)) {
        t.row(vec![i.name.clone(), i.state.clone(), i.addresses.first().map_or("-", String::as_str).to_string(), i.mac.clone()]);
        for a in i.addresses.iter().skip(1) {
            t.row(vec![String::new(), String::new(), a.clone()]);
        }
    }
    Ok(t.render(&Ui::plain_at(None)))
}

/// `show system storage`: each filesystem's size and use, and the system versions kept on it.
pub fn storage(f: &Facts, ui: &Ui) -> String {
    let mut t = Table::new(&[Col::Left("Filesystem"), Col::Right("Size"), Col::Right("Used"), Col::Right("Free"), Col::Right("Use"), Col::Left("Mounted on")]);
    for s in &f.filesystems {
        t.row(vec![s.device.clone(), size(s.size), size(s.used), size(s.free), format!("{}%", (s.used * 100).checked_div(s.size).unwrap_or(0)), s.mount.clone()]);
    }
    let mut out = t.render(ui);
    let kept = format!(
        "System versions kept: {} (each one a configuration the machine can start; the regular clean-up, nix.gc in the configuration, removes old ones).",
        f.generations
    );
    out.push_str(&format!("{}\n", ui.line("", &kept)));
    out
}

/// `show users`: the people with an account here, where each is logged in now, and their groups —
/// and where a person's groups are set, since the groups are what is changed most.
pub fn users(f: &Facts, ui: &Ui) -> String {
    let mut t = Table::new(&[Col::Left("User"), Col::Left("Logged in"), Col::Text("Groups")]);
    for p in &f.people {
        t.row(vec![p.name.clone(), p.sessions.first().map_or("-", String::as_str).to_string(), p.groups.join(" ")]);
        for s in p.sessions.iter().skip(1) {
            t.row(vec![String::new(), s.clone()]);
        }
    }
    let mut out = t.render(ui);
    let how = "A person's groups: `set users users <name> extraGroups <group>` in configuration mode (`docker`, say, once `virtualisation docker enable true`).";
    out.push_str(&format!("{}\n", ui.line("", how)));
    out
}

/// The people in /etc/passwd and /etc/group, as the files have them: a person's account (NixOS's
/// isNormalUser) has a number from 1000, below the build accounts' (30000 on).
pub fn people(passwd: &str, group: &str) -> Vec<Person> {
    let groups: Vec<(&str, &str, Vec<&str>)> = group
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            Some((*f.first()?, *f.get(2)?, f.get(3)?.split(',').filter(|m| !m.is_empty()).collect()))
        })
        .collect();
    passwd
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            let (name, id, own) = (*f.first()?, f.get(2)?.parse::<u32>().ok()?, *f.get(3)?);
            if !(1000..30000).contains(&id) {
                return None;
            }
            let theirs = groups.iter().filter(|(_, g, _)| *g == own).chain(groups.iter().filter(|(_, g, m)| *g != own && m.contains(&name)));
            Some(Person { name: name.to_string(), groups: theirs.map(|(n, _, _)| n.to_string()).collect(), sessions: vec![] })
        })
        .collect()
}

/// Logins, from `loginctl show-session <ids> -p Name -p Class -p TTY -p RemoteHost` (a block of
/// `Key=value` lines per session): whose, and where (`pts/0 from 192.168.64.1`). A person's own
/// service manager (class `manager`) is not a login.
pub fn sessions(show: &str) -> Vec<(String, String)> {
    show.split("\n\n")
        .filter_map(|block| {
            let get = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix('=')).unwrap_or_default();
            if !get("Class").starts_with("user") {
                return None;
            }
            let from = Some(get("RemoteHost")).filter(|h| !h.is_empty()).map(|h| format!("from {h}"));
            let at: Vec<String> = Some(get("TTY")).filter(|t| !t.is_empty()).map(String::from).into_iter().chain(from).collect();
            Some((get("Name").to_string(), if at.is_empty() { "a session".into() } else { at.join(" ") }))
        })
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

/// Two columns: a name, then its value.
fn rows(r: &[(&str, String)], ui: &Ui) -> String {
    let mut t = Table::new(&[Col::Left(""), Col::Text("")]);
    for (k, v) in r {
        t.row(vec![k.to_string(), v.clone()]);
    }
    t.render(ui)
}

/// `1 hour 32 minutes`, `3 days 2 hours`, `45 seconds`: the two largest parts.
pub fn duration(secs: u64) -> String {
    let parts = [(secs / 86_400, "day"), (secs / 3600 % 24, "hour"), (secs / 60 % 60, "minute"), (secs % 60, "second")];
    let shown: Vec<String> = parts.iter().skip_while(|(n, _)| *n == 0).take(2).filter(|(n, _)| *n > 0).map(|(n, u)| format!("{n} {u}{}", if *n == 1 { "" } else { "s" })).collect();
    if shown.is_empty() { "0 seconds".into() } else { shown.join(" ") }
}

/// Bytes as people read a disk's size: 62G, 87M, 1.5T.
pub fn size(bytes: u64) -> String {
    let units = ["B", "K", "M", "G", "T", "P"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v < 10.0 && u > 0 { format!("{v:.1}{}", units[u]) } else { format!("{v:.0}{}", units[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            nixos: "26.05.20261029.6f06d20".into(),
            kernel: "6.18.59".into(),
            uptime: 5519,
            load: [0.1, 0.25, 1.0],
            interfaces: vec![
                Interface { name: "enp0s1".into(), state: "up".into(), mac: "ae:21:5a:0b:a0:55".into(), addresses: vec!["192.168.64.4/24".into(), "fd5c::1/64".into()] },
                Interface { name: "lo".into(), state: "unknown".into(), mac: "00:00:00:00:00:00".into(), addresses: vec![] },
            ],
            filesystems: vec![Filesystem { device: "/dev/vda2".into(), mount: "/".into(), size: 62 << 30, used: 33 << 30, free: 27 << 30 }],
            generations: 23,
            people: vec![],
        }
    }

    #[test]
    fn what_the_machine_runs_and_how_it_is() {
        let f = facts();
        let v = version(&f, "torq 2026-11-01", &Ui::plain());
        assert!(v.starts_with("TorqOS        torq 2026-11-01\nNixOS         26.05.20261029.6f06d20\nLinux kernel  6.18.59\ntorqcli       "), "{v}");
        let u = uptime(&f, Some("#85, 2026-11-01 20:10: M2"), &Ui::plain());
        assert_eq!(u, "Up           1 hour 31 minutes\nLoad         0.10 0.25 1.00 (over 1, 5 and 15 minutes)\nLast change  #85, 2026-11-01 20:10: M2\n");
        let i = interfaces(&f, None).unwrap();
        assert_eq!(i, "Interface  State    Address          Hardware address\nenp0s1     up       192.168.64.4/24  ae:21:5a:0b:a0:55\n                    fd5c::1/64\nlo         unknown  -                00:00:00:00:00:00\n");
        assert_eq!(interfaces(&f, Some("lo")).unwrap(), "Interface  State    Address  Hardware address\nlo         unknown  -        00:00:00:00:00:00\n", "the one named");
        assert_eq!(format!("{:#}", interfaces(&f, Some("enp0s2")).unwrap_err()), "'enp0s2' is not an interface of this machine; did you mean enp0s1? (one of: enp0s1, lo)");
        let s = storage(&f, &Ui::plain());
        assert!(s.starts_with("Filesystem  Size  Used  Free  Use  Mounted on\n/dev/vda2    62G   33G   27G  53%  /\nSystem versions kept: 23"), "{s}");
        assert_eq!((duration(45), duration(3600), duration(90061), duration(0)), ("45 seconds".into(), "1 hour".into(), "1 day 1 hour".into(), "0 seconds".into()));
        assert_eq!((size(512), size(87 << 20), size(1536 << 30)), ("512B".into(), "87M".into(), "1.5T".into()));
    }

    #[test]
    fn the_people_here_their_groups_and_logins() {
        let passwd = "root:x:0:0::/root:/bin/sh\nviper:x:1000:100::/home/viper:/bin/zsh\nsam:x:1001:100::/home/sam:/bin/zsh\nnixbld1:x:30001:30000::/var/empty:/bin/nologin\nsshd:x:998:998::/var/empty:/bin/nologin\n";
        let group = "root:x:0:\nwheel:x:1:viper\nusers:x:100:\ndocker:x:131:sam,viper\nnixbld:x:30000:nixbld1\n";
        let mut people = people(passwd, group);
        assert_eq!(people.iter().map(|p| (p.name.as_str(), p.groups.join(" "))).collect::<Vec<_>>(), [("viper", "users wheel docker".into()), ("sam", "users docker".into())], "people only, their own group first");

        let show = "Name=viper\nClass=user\nTTY=pts/0\nRemoteHost=192.168.64.1\n\nName=viper\nClass=manager\nTTY=\nRemoteHost=\n\nName=viper\nClass=user\nTTY=tty1\nRemoteHost=\n\nName=sam\nClass=user\nTTY=\nRemoteHost=10.0.0.9\n";
        let s = sessions(show);
        assert_eq!(s, [("viper".into(), "pts/0 from 192.168.64.1".into()), ("viper".into(), "tty1".into()), ("sam".into(), "from 10.0.0.9".into())], "a service manager is not a login");

        people[0].sessions = vec!["pts/0 from 192.168.64.1".into(), "tty1".into()];
        let f = Facts { people, ..Default::default() };
        let u = users(&f, &Ui::plain());
        assert!(u.starts_with("User   Logged in                Groups\nviper  pts/0 from 192.168.64.1  users wheel docker\n       tty1\nsam    -                        users docker\nA person's groups: `set users users <name> extraGroups <group>`"), "{u}");
    }
}
