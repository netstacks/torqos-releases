//! Permission tiers and path policy.
//!
//! The tier of a change is the highest tier of any file it touches. Paths are relative to the
//! config repo root. This mapping is deliberately simple and conservative for M1: anything not
//! recognised is `System`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Observe,
    User,
    System,
    Forbidden,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Tier::Observe => "observe",
            Tier::User => "user",
            Tier::System => "system",
            Tier::Forbidden => "forbidden",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    User,
    Agent,
}

impl fmt::Display for Actor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Actor::User => "user",
            Actor::Agent => "agent",
        })
    }
}

/// `--actor user|agent` (clap's ValueEnum derive would write an `allow(dead_code)`, which this crate
/// forbids).
impl std::str::FromStr for Actor {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "user" => Ok(Actor::User),
            "agent" => Ok(Actor::Agent),
            _ => Err(format!("'{s}': one of user, agent")),
        }
    }
}

/// Paths the agent may never change, no matter what. Changing them requires actor=user, and in
/// the real product only through a UI the user operates directly.
const FORBIDDEN_PREFIXES: &[&str] = &[
    "policy/",      // AI permissions and tier rules
    "secureboot/",  // key enrollment settings
    "ledger/",      // anything about audit configuration
];

/// The per-user data files that are user tier: `users/<name>/<leaf>`. An explicit allow-list, not
/// a directory: a leaf a later release starts reading (say `ssh-keys.json`) must not become user
/// tier by accident, and a file planted today must not become active then. Any `.nix` file is a
/// full NixOS module (sudo rules, services, ...) wherever it lives, so it is system tier. Keep in
/// step with broker/user-leaves.json, which modules/packages.nix reads (tested below). A leaf is
/// added only together with the typed module that reads it, and with its shape in `user_leaf_ok`:
/// `packages.json` (modules/packages.nix).
pub const USER_LEAVES: &[&str] = &["packages.json"];

/// The most packages one user's list may hold: a lightly-approved change must not be able to start
/// hours of downloads and builds while holding the broker lock.
pub const MAX_USER_PACKAGES: usize = 256;

/// Whether `value` has exactly the shape the user leaf `leaf` takes. Checked by the broker before
/// accepting a proposal, so data the leaf's module would reject is refused up front instead of
/// failing the rebuild; the module's own types remain the authority on meaning.
pub fn user_leaf_ok(leaf: &str, value: &serde_json::Value) -> bool {
    match leaf {
        // A list of nixpkgs attribute names.
        "packages.json" => value.as_array().is_some_and(|names| {
            names.len() <= MAX_USER_PACKAGES && names.iter().all(|n| n.as_str().is_some_and(package_name_ok))
        }),
        _ => false,
    }
}

/// The form of a package name, mirroring modules/packages.nix (`isAttrName`, depth): a top-level
/// nixpkgs attribute or one level under a package set. Which sets are allowed, and whether the
/// package exists and is available, is the module's call at evaluation.
fn package_name_ok(name: &str) -> bool {
    let attr = |s: &str| {
        let mut c = s.chars();
        matches!(c.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
            && c.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '\'' | '+' | '-'))
    };
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() <= 2 && parts.iter().all(|p| attr(p))
}

/// User tier: exactly `users/<name>/<leaf>` with an allow-listed leaf. The broker also checks the
/// content of such a file before accepting it (`Broker::check_user_data`).
pub fn is_user_data(p: &str) -> bool {
    let Some((name, leaf)) = p.strip_prefix("users/").and_then(|rest| rest.split_once('/')) else {
        return false;
    };
    user_name_ok(name) && USER_LEAVES.contains(&leaf)
}

/// A password hash in crypt's form, whole: `$6$[rounds=N$]salt$hash` (SHA-512), `$y$params$salt$hash`
/// (yescrypt), `$2b$cost$salt+hash` (bcrypt) — not only its prefix: `$6$` alone would be set as it
/// is and lock the account out (audit of 2026-11-01, N11).
pub fn password_hash_ok(hash: &str) -> bool {
    let crypt = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'/');
    let parts: Vec<&str> = hash.split('$').collect();
    hash.len() < 512
        && match parts.as_slice() {
            ["", "6", salt, h] => crypt(salt) && crypt(h),
            ["", "6", rounds, salt, h] => rounds.strip_prefix("rounds=").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) && crypt(salt) && crypt(h),
            ["", "y", params, salt, h] => crypt(params) && crypt(salt) && crypt(h),
            ["", "2b", cost, rest] => cost.len() == 2 && cost.bytes().all(|b| b.is_ascii_digit()) && rest.len() == 53 && crypt(rest),
            _ => false,
        }
}

/// A conservative user name: what `useradd` accepts by default, minus uppercase.
pub fn user_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z' | '_')) && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

pub fn tier_for_path(path: &str) -> Tier {
    let p = path.trim_start_matches("./");
    if p.contains("..") {
        return Tier::Forbidden;
    }
    // Git's own files in the tree (.gitattributes, .gitignore, .gitmodules) are no configuration, and
    // change how git shows and applies everything else (the second review of ADR-041).
    if p.split('/').any(|c| c.starts_with(".git")) {
        return Tier::Forbidden;
    }
    if FORBIDDEN_PREFIXES.iter().any(|f| p.starts_with(f)) {
        return Tier::Forbidden;
    }
    if is_user_data(p) {
        return Tier::User;
    }
    Tier::System
}

pub fn tier_for_paths<S: AsRef<str>>(paths: &[S]) -> Tier {
    paths
        .iter()
        .map(|p| tier_for_path(p.as_ref()))
        .max()
        .unwrap_or(Tier::Observe)
}

pub fn check_allowed(tier: Tier) -> Result<()> {
    // For everyone, here, where every way in passes: the socket refusing it was one door of several
    // (sudo, setup, recovery), and a new one would have had to know (the review of 0.2.1, M2).
    if tier == Tier::Forbidden {
        bail!("refused: no one may change forbidden paths (policy, secure boot, the ledger, git's own files)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_hash_is_whole() {
        for good in ["$6$salt$abcDEF012./", "$6$rounds=5000$salt$abc", "$y$j9T$salt$hash", &format!("$2b$12${}", "a".repeat(53))] {
            assert!(password_hash_ok(good), "{good}");
        }
        for bad in ["$6$", "$6$salt", "$6$salt$", "$y$j9T$$hash", "$6$sa:lt$h", "$6$rounds=$s$h", "$2b$12$short", "!", "", "$1$salt$hash"] {
            assert!(!password_hash_ok(bad), "{bad}");
        }
    }


    #[test]
    fn classifies() {
        assert_eq!(tier_for_path("users/casey/packages.json"), Tier::User);
        assert_eq!(tier_for_path("./users/casey/packages.json"), Tier::User);
        assert_eq!(tier_for_path("users/casey/cli.json"), Tier::System, "no longer read: torqcli speaks one syntax (ADR-021)");
        assert_eq!(tier_for_path("users/casey/env.json"), Tier::System, "no leaf without a typed reader");
        assert_eq!(tier_for_path("users/casey.nix"), Tier::System, "a NixOS module is never user tier");
        assert_eq!(tier_for_path("desktop.nix"), Tier::System);
        assert_eq!(tier_for_path("policy/tiers.toml"), Tier::Forbidden);
        assert_eq!(tier_for_path("users/../policy/x"), Tier::Forbidden);
        assert_eq!(tier_for_paths(&["users/a.nix", "system.nix"]), Tier::System);
        assert_eq!(tier_for_paths::<&str>(&[]), Tier::Observe);
    }

    /// Only allow-listed per-user data files are user tier. Anything that could be a module, any
    /// other leaf (a later `ssh-keys.json` must not become user tier by accident), and anything that
    /// escapes the exact `users/<name>/<leaf>` shape is system tier.
    #[test]
    fn user_tier_is_an_allow_list_of_data_files() {
        for p in [
            "users/casey.nix",
            "users/default.nix",
            "users/casey/default.nix",
            "users/casey/packages.nix",
            "users/casey/ssh-keys.json",
            "users/casey/environment.json",
            "users/casey/packages.json.bak",
            "users/casey/sub/packages.json",
            "users/casey/packages.json/x",
            "users/casey.packages.json",
            "users/.hidden/packages.json",
            "users/-x/packages.json",
            "users/Casey/packages.json",
            "users/ca sey/packages.json",
            "users//packages.json",
            "users/packages.json",
            "themes/dark.json",
            "users",
        ] {
            assert_eq!(tier_for_path(p), Tier::System, "{p}");
        }
        for p in ["users/casey/packages.json", "users/_svc/packages.json", "users/a-b_9/packages.json"] {
            assert_eq!(tier_for_path(p), Tier::User, "{p}");
        }
        assert_eq!(USER_LEAVES, ["packages.json"]);
        // The modules read the same list (modules/packages.nix): the two cannot drift apart.
        let shared: Vec<String> = serde_json::from_str(include_str!("../user-leaves.json")).unwrap();
        assert_eq!(USER_LEAVES, shared, "broker/user-leaves.json");
    }

    /// Each user leaf has exactly one shape; anything else is refused at propose rather than failing
    /// at the rebuild.
    #[test]
    fn user_leaf_shapes() {
        let ok = |leaf: &str, json: &str| user_leaf_ok(leaf, &serde_json::from_str(json).unwrap());
        assert!(ok("packages.json", r#"["htop", "python3Packages.requests", "g++", "vim_configurable", "xorg'"]"#));
        assert!(ok("packages.json", "[]"));
        for bad in ["{}", "[1]", "[{}]", "[null]", "\"htop\"", "42"] {
            assert!(!ok("packages.json", bad), "{bad}");
        }
        // Names the module would reject anyway: not an attribute name, or deeper than one set.
        for bad in [r#"["../x"]"#, r#"["a.b.c"]"#, r#"[""]"#, r#"[".htop"]"#, r#"["htop."]"#, r#"["1password"]"#, r#"["a b"]"#, r#"["${x}"]"#] {
            assert!(!ok("packages.json", bad), "{bad}");
        }
        let at_most = |n: usize| format!("[{}]", vec!["\"p\""; n].join(","));
        assert!(ok("packages.json", &at_most(256)));
        assert!(!ok("packages.json", &at_most(257)));
        assert!(!ok("unknown.json", "[]"), "a leaf without a shape is never ok");
        assert!(!ok("cli.json", r#"{"style": "j"}"#), "no longer a user leaf (ADR-021)");
    }

    /// The second review of ADR-041: git's own files change how git shows and applies the rest.
    #[test]
    fn gits_own_files_are_no_ones() {
        for p in [".gitattributes", "nixos/.gitattributes", "users/casey/.gitignore", ".gitmodules"] {
            assert_eq!(tier_for_path(p), Tier::Forbidden, "{p}");
        }
        assert_eq!(tier_for_path("nixos/services.json"), Tier::System);
    }

    #[test]
    fn no_one_touches_forbidden() {
        assert!(check_allowed(Tier::Forbidden).is_err());
        assert!(check_allowed(Tier::Forbidden).is_err(), "a person's too: sudo, setup and recovery go through here");
        assert!(check_allowed(Tier::System).is_ok() && check_allowed(Tier::User).is_ok());
    }
}
