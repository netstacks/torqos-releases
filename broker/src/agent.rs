//! What the agent may propose (ADR-041): a default-deny list, checked by what a change does.
//!
//! The agent's account (`torq-agent`) only proposes (ADR-040); this decides which proposals it may
//! make at all. A person's packages: the people `users` names (the owner, by default). Settings:
//! only the subtrees `settings` lists, checked setting by setting — each JSON file of the tree is
//! compared before and after the change, and every setting that changes must lie inside one of
//! them. Anything else — the config files and the Nix files they attach, `flake.nix`, the lock, the
//! hardware file, and `torq.*` (where the agent's own limits are set) — is never the agent's.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The agent's limits, from /etc/torq/broker.json (`torq.agent.mayPropose`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// The people whose packages (`users/<name>/packages.json`) the agent may propose.
    #[serde(default)]
    pub users: Vec<String>,
    /// The settings subtrees it may propose changes in, as dotted paths (`services.openssh`,
    /// `system.host`). `torq` and anything under it is never one, whatever is listed.
    #[serde(default)]
    pub settings: Vec<String>,
}

/// One file the change touches: its path in the repo, and its JSON before and after (None: absent).
pub struct Touched {
    pub path: String,
    pub before: Option<Value>,
    pub after: Option<Value>,
}

/// The tree's top-level that is never the agent's: the agent's own limits are in it.
const NEVER: &str = "torq";

/// Which tree file holds `path`'s settings, and the dotted prefix they sit under: `nixos/<top>.json`
/// holds `<top>.…`, `system/<name>.json` holds `system.<name>.…` (torqcli's own tree, cli/src/tree.rs).
fn settings_prefix(path: &str) -> Option<Vec<String>> {
    let (dir, file) = path.split_once('/')?;
    let stem = file.strip_suffix(".json")?;
    if stem.is_empty() || stem.contains('/') || stem.starts_with('.') {
        return None;
    }
    match dir {
        "nixos" => Some(vec![stem.to_string()]),
        "system" => Some(vec!["system".to_string(), stem.to_string()]),
        _ => None,
    }
}

/// Every setting that differs between `a` and `b`, as path segments under `at`: inside objects,
/// key by key; anything else (a value, a list) is one setting, changed or not.
fn changed(at: &[String], a: Option<&Value>, b: Option<&Value>, out: &mut Vec<Vec<String>>) {
    match (a, b) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let mut p = at.to_vec();
                p.push(k.clone());
                changed(&p, x.get(k), y.get(k), out);
            }
        }
        (Some(Value::Object(x)), None) | (None, Some(Value::Object(x))) if !x.is_empty() => {
            for (k, v) in x {
                let mut p = at.to_vec();
                p.push(k.clone());
                let (a2, b2) = if a.is_some() { (Some(v), None) } else { (None, Some(v)) };
                changed(&p, a2, b2, out);
            }
        }
        (x, y) if x != y => out.push(at.to_vec()),
        _ => {}
    }
}

/// Whether `setting` lies inside one of `allowed` (segment by segment: `services.openssh` covers
/// `services.openssh.settings`, not `services.openssh2`, and not a key that is itself "openssh.x").
fn inside(setting: &[String], allowed: &[String]) -> bool {
    if setting.first().map(String::as_str) == Some(NEVER) {
        return false;
    }
    allowed.iter().any(|a| {
        let a: Vec<&str> = a.split('.').collect();
        // A bare `system` is two trees at once (torqcli's, and NixOS's system.activationScripts): never.
        !a.is_empty() && a[0] != NEVER && a != ["system"] && a.iter().all(|s| !s.is_empty()) && setting.len() >= a.len() && a.iter().zip(setting).all(|(x, y)| *x == y)
    })
}

/// Why the agent may not propose this change, or None if it may.
pub fn refusal(limits: &Limits, touched: &[Touched]) -> Option<String> {
    for t in touched {
        if let Some(name) = t.path.strip_prefix("users/").and_then(|r| r.strip_suffix("/packages.json")) {
            if crate::policy::is_user_data(&t.path) && limits.users.iter().any(|u| u == name) {
                continue;
            }
            return Some(format!("{}: the agent proposes packages only for {}", t.path, list(&limits.users)));
        }
        let Some(prefix) = settings_prefix(&t.path) else {
            return Some(format!("{}: not a setting — config files, flake.nix, the lock and the hardware file are never the agent's", t.path));
        };
        let mut settings = Vec::new();
        changed(&prefix, t.before.as_ref(), t.after.as_ref(), &mut settings);
        if let Some(s) = settings.iter().find(|s| !inside(s, &limits.settings)) {
            return Some(format!(
                "{}: the agent may not change {} (it proposes settings only in {})",
                t.path,
                s.join("."),
                list(&limits.settings)
            ));
        }
    }
    None
}

/// The settings whose change could lose the network (and with it a person reaching the machine) or
/// the screen (ADR-042, 6): an agent's change in one of them is applied as a commit confirmed, which
/// rolls itself back unless the person, still able to reach the machine, confirms it. `boot` and
/// `hardware` act at the next start too: the boot default is not moved until the commit is confirmed.
pub const CUTS_OFF: &[&str] = &[
    // The network, and the ways in over it.
    "networking",
    "systemd.network",
    "services.openssh",
    "services.sshd",
    "services.sshguard",
    "services.resolved",
    "services.tailscale",
    "users.users.*.openssh",
    "users.extraUsers.*.openssh",
    // What runs, and what a unit or a file in /etc can change of all of it.
    "systemd.services",
    "environment.etc",
    // The machine itself, and how it starts.
    "hardware",
    "boot",
    // The screen, and the console.
    "console",
    "services.getty",
    "services.kmscon",
    "services.xserver",
    "services.displayManager",
    "services.desktopManager",
    "services.greetd",
    "programs.niri",
];

/// Whether `setting` lies inside `pattern` (dotted, segment by segment; `*` is any one segment).
fn under(setting: &[String], pattern: &str) -> bool {
    let p: Vec<&str> = pattern.split('.').collect();
    setting.len() >= p.len() && p.iter().zip(setting).all(|(a, s)| *a == "*" || a == s)
}

/// The settings of `touched` that lie inside `CUTS_OFF`, as dotted paths (none: an empty list).
pub fn cuts_off(touched: &[Touched]) -> Vec<String> {
    let mut out = Vec::new();
    for t in touched {
        let Some(prefix) = settings_prefix(&t.path) else { continue };
        let mut settings = Vec::new();
        changed(&prefix, t.before.as_ref(), t.after.as_ref(), &mut settings);
        out.extend(settings.iter().filter(|s| CUTS_OFF.iter().any(|p| under(s, p))).map(|s| s.join(".")));
    }
    out
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "nothing, until the owner lists some (torq.agent.mayPropose)".into()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn limits(users: &[&str], settings: &[&str]) -> Limits {
        Limits { users: users.iter().map(|s| s.to_string()).collect(), settings: settings.iter().map(|s| s.to_string()).collect() }
    }
    fn t(path: &str, before: Option<Value>, after: Option<Value>) -> Touched {
        Touched { path: path.into(), before, after }
    }

    #[test]
    fn the_owners_packages_by_default_and_no_one_elses() {
        let l = limits(&["viper"], &[]);
        assert_eq!(refusal(&l, &[t("users/viper/packages.json", None, Some(json!(["htop"])))]), None);
        assert!(refusal(&l, &[t("users/sam/packages.json", None, Some(json!(["htop"])))]).is_some());
        assert!(refusal(&l, &[t("users/viper/other.json", None, Some(json!({})))]).is_some());
    }

    #[test]
    fn no_setting_until_the_owner_lists_it() {
        let l = limits(&["viper"], &[]);
        let r = refusal(&l, &[t("nixos/services.json", Some(json!({})), Some(json!({"openssh": {"enable": true}})))]).unwrap();
        assert!(r.contains("services.openssh.enable") && r.contains("until the owner"), "{r}");
    }

    #[test]
    fn a_listed_subtree_and_nothing_beside_it() {
        let l = limits(&[], &["services.openssh"]);
        let before = json!({"openssh": {"enable": false}, "nginx": {"enable": false}});
        assert_eq!(refusal(&l, &[t("nixos/services.json", Some(before.clone()), Some(json!({"openssh": {"enable": true}, "nginx": {"enable": false}})))]), None);
        // One allowed change beside one that is not: refused.
        let both = json!({"openssh": {"enable": true}, "nginx": {"enable": true}});
        assert!(refusal(&l, &[t("nixos/services.json", Some(before.clone()), Some(both))]).unwrap().contains("services.nginx.enable"));
        // Segment by segment: not openssh2, not a key that is itself "openssh.x".
        assert!(refusal(&l, &[t("nixos/services.json", Some(json!({})), Some(json!({"openssh2": {"enable": true}})))]).is_some());
        assert!(refusal(&l, &[t("nixos/services.json", Some(json!({})), Some(json!({"openssh.x": true})))]).is_some());
        // Removing what is listed is a change in it too; removing what is not, is refused.
        assert_eq!(refusal(&l, &[t("nixos/services.json", Some(json!({"openssh": {"enable": true}})), Some(json!({})))]), None);
        assert!(refusal(&l, &[t("nixos/services.json", Some(json!({"nginx": {"enable": true}})), None)]).is_some());
    }

    #[test]
    fn torq_and_the_files_that_are_not_settings_are_never_the_agents() {
        // Even listed: torq.* holds the agent's own limits.
        let l = limits(&["viper"], &["torq", "torq.agent", "services"]);
        assert!(refusal(&l, &[t("nixos/torq.json", Some(json!({})), Some(json!({"agent": {"mayPropose": {"settings": ["nix"]}}})))]).is_some());
        for f in ["flake.nix", "flake.lock", "config-files.json", "hardware-configuration.nix", "nixos/x.nix", "nixos/sub/services.json", "nixos/.json", "secrets/x.json"] {
            assert!(refusal(&l, &[t(f, None, Some(json!({})))]).is_some(), "{f}");
        }
        // torq's own tree (system/*.json) by its own prefix.
        assert_eq!(refusal(&limits(&[], &["system.host"]), &[t("system/host.json", Some(json!({"host-name": "a"})), Some(json!({"host-name": "b"})))]), None);
        assert!(refusal(&limits(&[], &["system.host"]), &[t("system/packages.json", Some(json!([])), Some(json!(["jq"])))]).is_some());
    }

    /// ADR-042, 6: what could lose the network or the screen is named, and nothing else is.
    #[test]
    fn what_could_cut_the_machine_off() {
        let wifi = t("nixos/networking.json", Some(json!({"wireless": {"enable": false}})), Some(json!({"wireless": {"enable": true}})));
        assert_eq!(cuts_off(&[wifi]), ["networking.wireless.enable"]);
        let ssh = t("nixos/services.json", Some(json!({})), Some(json!({"openssh": {"settings": {"PasswordAuthentication": false}}, "nginx": {"enable": true}})));
        assert_eq!(cuts_off(&[ssh]), ["services.openssh.settings.PasswordAuthentication"]);
        assert!(cuts_off(&[t("nixos/services.json", Some(json!({})), Some(json!({"openssh2": {"enable": true}})))]).is_empty());
        assert!(cuts_off(&[t("users/viper/packages.json", None, Some(json!(["htop"])))]).is_empty());
        assert!(cuts_off(&[t("nixos/time.json", Some(json!({})), Some(json!({"timeZone": "UTC"})))]).is_empty());
        assert_eq!(cuts_off(&[t("nixos/boot.json", Some(json!({})), Some(json!({"kernelParams": ["quiet"]})))]), ["boot.kernelParams"]);
        // The review of w5: the aliases, a person's SSH keys, units, the console, a VPN, /etc.
        for (file, after, setting) in [
            ("nixos/services.json", json!({"sshd": {"enable": false}}), "services.sshd.enable"),
            ("nixos/services.json", json!({"sshguard": {"enable": true}}), "services.sshguard.enable"),
            ("nixos/services.json", json!({"tailscale": {"enable": false}}), "services.tailscale.enable"),
            ("nixos/services.json", json!({"getty": {"autologinUser": "x"}}), "services.getty.autologinUser"),
            ("nixos/services.json", json!({"kmscon": {"enable": true}}), "services.kmscon.enable"),
            ("nixos/users.json", json!({"users": {"dev": {"openssh": {"authorizedKeys": {"keys": []}}}}}), "users.users.dev.openssh.authorizedKeys.keys"),
            ("nixos/users.json", json!({"extraUsers": {"dev": {"openssh": {"authorizedKeys": {"keys": []}}}}}), "users.extraUsers.dev.openssh.authorizedKeys.keys"),
            ("nixos/systemd.json", json!({"services": {"sshd": {"enable": false}}}), "systemd.services.sshd.enable"),
            ("nixos/console.json", json!({"keyMap": "fr"}), "console.keyMap"),
            ("nixos/environment.json", json!({"etc": {"resolv.conf": {"text": ""}}}), "environment.etc.resolv.conf.text"),
        ] {
            assert_eq!(cuts_off(&[t(file, Some(json!({})), Some(after))]), [setting], "{setting}");
        }
        // `*` is one segment, and only where it is written: a person's other settings are not on it.
        assert!(cuts_off(&[t("nixos/users.json", Some(json!({})), Some(json!({"users": {"dev": {"shell": "zsh"}}})))]).is_empty());
        assert!(cuts_off(&[t("nixos/environment.json", Some(json!({})), Some(json!({"systemPackages": ["htop"]})))]).is_empty());
    }

    #[test]
    fn an_empty_or_odd_entry_allows_nothing() {
        assert!(refusal(&limits(&[], &["system"]), &[t("nixos/system.json", Some(json!({})), Some(json!({"activationScripts": {"x": "id"}})))]).is_some(), "a bare system");
        for bad in ["", ".", "services.", ".openssh"] {
            assert!(refusal(&limits(&[], &[bad]), &[t("nixos/services.json", Some(json!({})), Some(json!({"openssh": {"enable": true}})))]).is_some(), "{bad:?}");
        }
    }
}
