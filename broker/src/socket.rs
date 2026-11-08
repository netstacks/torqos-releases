//! The broker on a socket (ADR-040): who is asking is the kernel's answer, never the caller's.
//!
//! `torq-broker.socket` hands each connection to `torq-broker serve` (systemd, `Accept=yes`): the
//! connection is its standard input and output. The caller's uid comes from `SO_PEERCRED`; what it
//! asks for is one JSON line (`Request`); what it may do is decided here (`gate`), by that uid and
//! the change's tier; a system change asks the caller's password, checked through PAM. Then the
//! broker runs as it always has — as root, the request's arguments plus the identity this module
//! decided — and its output comes back as `Frame` lines, its exit code last.

use crate::policy::{self, Tier};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Where the socket is (modules/broker.nix sets the unit to it).
pub const SOCKET: &str = "/run/torq/broker.sock";
/// The agent's account (ADR-035): whatever arrives from it is the agent.
pub const AGENT: &str = "torq-agent";
/// The PAM service a system change's password is checked against (modules/broker.nix).
pub const PAM_SERVICE: &str = "torq-approve";
/// The group whose members may approve a system change (with their password).
pub const ADMINS: &str = "wheel";
/// The exit code that says: this needs your password (none was sent, or it was wrong).
pub const NEED_PASSWORD: i32 = 77;
/// A request is a line, and no line is longer than this (a large diff, with room).
pub const MAX_REQUEST: u64 = 8 << 20;
/// The uids NixOS gives normal accounts (people): `isNormalUser` starts at 1000; Nix's build users
/// start at 30001, and are not people.
pub const PEOPLE_UIDS: std::ops::Range<u32> = 1000..30000;
/// Where faillock counts a password's failures: root's alone (modules/broker.nix), so that a program
/// running as the person cannot reset its own count between guesses.
pub const FAILLOCK_DIR: &str = "/run/torq/faillock";
/// How long a caller has to send its request, and to take each piece of the reply, in seconds.
pub const TIMEOUT_SECONDS: u64 = 10;

/// What a caller sends: the broker's arguments, without who it is or any file. A diff or a
/// password hash comes in the request itself, never as a path for root to open (N16).
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// A backup's key (`backup-key`): its passphrase, kept root's alone, never in the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What comes back, one JSON line at a time: the broker's standard output and error as they come,
/// and how it ended, last.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Frame {
    Out(String),
    Err(String),
    Exit(i32),
}

/// The account at the other end of the socket, as the kernel and the machine's own files say.
#[derive(Debug, Clone, PartialEq)]
pub struct Caller {
    pub uid: u32,
    pub name: String,
    pub agent: bool,
    pub admin: bool,
    /// A normal account (NixOS gives people uids from 1000): not a service's, which proposes nothing.
    pub person: bool,
}

/// What a command needs from its caller.
#[derive(Debug, PartialEq)]
pub enum Gate {
    Open,
    Password,
    Refused(String),
}

/// The change a command acts on: its tier and the files it touches (a proposal's, for `apply`; an
/// applied change's, for `revert`).
#[derive(Debug)]
pub struct Change {
    pub tier: Tier,
    pub files: Vec<String>,
}

/// Flags no caller may send: the broker's settings (they are /etc/torq/broker.json's), who is
/// asking and who approves (the kernel's), and files (they come in the request).
const NOT_FROM_CALLERS: &[&str] = &[
    "--repo", "--state-dir", "--rebuild-cmd", "--build-cmd", "--activate-cmd", "--boot-cmd", "--schedule-cmd",
    "--commit-at-cmd", "--commit-at-cancel-cmd", "--current-system", "--system-profile", "--actor", "--approved-by", "--diff", "--hash-file", "--key-file",
];

/// Refuse arguments that name the caller, a file, or the broker's settings.
pub fn check_args(args: &[String]) -> Result<()> {
    for a in args {
        let flag = a.split_once('=').map_or(a.as_str(), |(f, _)| f);
        if NOT_FROM_CALLERS.contains(&flag) {
            bail!("{flag} is not taken from a caller: who is asking is the kernel's answer, and a file comes in the request");
        }
    }
    Ok(())
}

/// The account with `uid`, from /etc/passwd's text; an admin if /etc/group's `wheel` lists it.
pub fn caller(uid: u32, passwd: &str, group: &str) -> Result<Caller> {
    let name = passwd
        .lines()
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let id = f.nth(1)?.parse::<u32>().ok()?;
            (id == uid).then(|| name.to_string())
        })
        .next()
        .with_context(|| format!("no account has uid {uid}"))?;
    if !policy::user_name_ok(&name) {
        bail!("the account {name:?} is not one TorqOS acts for");
    }
    let admin = group.lines().any(|l| {
        let f: Vec<&str> = l.split(':').collect();
        f.len() == 4 && f[0] == ADMINS && f[3].split(',').any(|m| m == name)
    });
    Ok(Caller { uid, agent: name == AGENT, person: (PEOPLE_UIDS).contains(&uid), name, admin })
}

/// Read-only commands: open to every account (the ledger says what changed, never a secret: a
/// password's hash is kept outside it).
const OBSERVE: &[&str] = &["log", "verify", "status", "show", "remote", "snapshots", "backups", "scheduled", "session", "trust"];

/// What `cmd` needs from `who`: `change` is what it acts on (`apply`, `revert`), `user` whose
/// password it sets (`password`). An `apply --confirm` is `apply-confirmed`: while it waits, no
/// one else's change can be made, and only an administrator may hold the machine so.
pub fn gate(who: &Caller, cmd: &str, change: Option<&Change>, user: Option<&str>) -> Gate {
    let admins_only = || {
        if who.admin {
            Gate::Password
        } else {
            Gate::Refused(format!("{} is not an administrator ({ADMINS}): a system change is theirs to approve", who.name))
        }
    };
    if OBSERVE.contains(&cmd) {
        return Gate::Open;
    }
    if who.agent {
        // It proposes, and keeps the broker told of what its sessions read (session.rs).
        return if matches!(cmd, "propose" | "session-start" | "session-taint") {
            Gate::Open
        } else {
            Gate::Refused("the agent proposes; a person applies".into())
        };
    }
    // A service's account (nobody, a daemon's) proposes nothing: the ledger is people's, and the
    // agent's, and every proposal stays in it for good.
    if !who.person {
        return Gate::Refused(format!("{} is not a person's account: only people, and the agent, propose changes", who.name));
    }
    match cmd {
        "propose" => Gate::Open,
        // A session is the agent's: its record is its reports, and no one else's.
        "session-start" | "session-taint" => Gate::Refused("a session is the agent's: a person's reads are their own".into()),
        // Trust in the agent with their own folder: theirs alone to give, with their password; ending it, at any time.
        "trust-grant" => Gate::Password,
        "trust-revoke" => Gate::Open,
        // A person's, not everyone's: it goes to the network, as root (the review of pull).
        "fetch-incoming" => Gate::Open,
        "apply-confirmed" => match change {
            Some(c) if c.tier == Tier::Forbidden => Gate::Refused("that change touches what no one may change".into()),
            Some(_) => admins_only(),
            None => Gate::Refused("no such change".into()),
        },
        // A commit at is approved now, as an apply is: the same person, the same password.
        "apply" | "revert" | "schedule" => match change {
            None => Gate::Refused("no such change".into()),
            Some(c) if c.tier == Tier::Forbidden => Gate::Refused("that change touches what no one may change".into()),
            // A person's own settings are theirs: no password (KNOWN-GAPS 6a — someone else's are not).
            Some(c) if theirs(&who.name, c) => Gate::Open,
            Some(_) => admins_only(),
        },
        // Their own password: the current one first, as passwd asks.
        "password" if user == Some(who.name.as_str()) => Gate::Password,
        // A commit at, by the one who approved it (`user`: theirs); anyone else's, an administrator's.
        "cancel-scheduled" if user == Some(who.name.as_str()) => Gate::Open,
        "cancel-scheduled" => admins_only(),
        "password" | "confirm" | "reconcile" | "repair" | "remote-key" | "remote-clear" | "restore-data" | "ledger-acknowledge" | "snapshot-take" | "snapshot-delete" | "backup-key" | "backup-restore" => admins_only(),
        other => Gate::Refused(format!("`{other}` is not run over the socket (it is the machine's own, as root)")),
    }
}

/// Whether change `c` is wholly within `name`'s own folder of the configuration: theirs to make.
pub fn theirs(name: &str, c: &Change) -> bool {
    c.tier <= Tier::User && !c.files.is_empty() && c.files.iter().all(|f| own(name, f))
}

/// The account named `name`, as `caller` reads it for its uid.
pub fn account(name: &str, passwd: &str, group: &str) -> Result<Caller> {
    let uid = passwd
        .lines()
        .find_map(|l| {
            let mut f = l.split(':');
            (f.next()? == name).then(|| f.nth(1)?.parse::<u32>().ok()).flatten()
        })
        .with_context(|| format!("no account is named {name:?}"))?;
    let c = caller(uid, passwd, group)?;
    if c.name != name {
        bail!("{name:?} shares its uid with {:?}: not one account", c.name);
    }
    Ok(c)
}

/// Whether `file` is in `name`'s own folder of the configuration.
fn own(name: &str, file: &str) -> bool {
    policy::is_user_data(file) && file.strip_prefix("users/").and_then(|r| r.split_once('/')).is_some_and(|(n, _)| n == name)
}

/// The arguments the broker runs with: the caller's, then who it is, as this module decided.
/// `diff`, `hash` and `key` are the private files the request's own came to.
pub fn with_identity(args: &[String], cmd: &str, who: &Caller, diff: Option<&str>, hash: Option<&str>, key: Option<&str>) -> Result<Vec<String>> {
    let mut a = args.to_vec();
    match cmd {
        "propose" => {
            // What the gate sees of a proposal is what it does on the commit it was judged on: without one,
            // a diff can be nothing at that commit and something at the next (the audit of M3, H1).
            if !args.iter().any(|x| x == "--base" || x.starts_with("--base=")) {
                bail!("a proposal needs --base, the commit it was prepared against");
            }
            a.extend(["--actor".into(), if who.agent { "agent".into() } else { "user".into() }]);
            a.extend(["--diff".into(), diff.context("a proposal needs its diff, in the request")?.into()]);
        }
        "password" => {
            a.extend(["--hash-file".into(), hash.context("a password needs its hash, in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        "backup-key" => {
            a.extend(["--key-file".into(), key.context("a backup's key comes in the request")?.into()]);
            a.extend(["--approved-by".into(), who.name.clone()]);
        }
        "apply" | "revert" | "schedule" | "cancel-scheduled" | "trust-grant" | "trust-revoke" | "confirm" | "reconcile" | "repair" | "remote-clear" | "restore-data" | "ledger-acknowledge" | "snapshot-take" | "snapshot-delete" | "backup-restore" => a.extend(["--approved-by".into(), who.name.clone()]),
        _ => {}
    }
    Ok(a)
}

/// Check `name`'s password through PAM (`torq-approve`: pam_unix, with faillock counting the
/// failures), as the broker — root — can. Nothing is kept: the password goes when this returns.
pub fn password_ok(name: &str, password: &str) -> bool {
    use nonstick::items::Items;
    use nonstick::{AuthnFlags, ConversationAdapter, PamShared, Transaction, TransactionBuilder};
    use std::ffi::{OsStr, OsString};
    struct Answer<'a> {
        name: &'a str,
        password: &'a str,
    }
    impl ConversationAdapter for Answer<'_> {
        fn prompt(&self, _: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
            Ok(self.name.into())
        }
        fn masked_prompt(&self, _: impl AsRef<OsStr>) -> nonstick::Result<OsString> {
            Ok(self.password.into())
        }
        fn error_msg(&self, _: impl AsRef<OsStr>) {}
        fn info_msg(&self, _: impl AsRef<OsStr>) {}
    }
    let Ok(mut t) = TransactionBuilder::new_with_service(PAM_SERVICE).username(name).build(Answer { name, password }.into_conversation()) else {
        return false;
    };
    // The account as PAM ended up with: a module may change it, and then it is not this person.
    t.authenticate(AuthnFlags::DISALLOW_NULL_AUTHTOK).is_ok()
        && t.account_management(AuthnFlags::DISALLOW_NULL_AUTHTOK).is_ok()
        && t.items().user().is_ok_and(|u| u.as_deref() == Some(OsStr::new(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str = "root:x:0:0::/root:/bin/sh\nviper:x:1000:100::/home/viper:/bin/zsh\nsam:x:1001:100::/home/sam:/bin/zsh\ntorq-agent:x:990:990::/var/empty:/run/current-system/sw/bin/nologin\nnobody:x:65534:65534::/var/empty:/run/current-system/sw/bin/nologin\nsshd:x:998:998::/var/empty:/run/current-system/sw/bin/nologin\n";
    const GROUP: &str = "root:x:0:\nwheel:x:1:viper\nusers:x:100:sam\n";

    fn who(uid: u32) -> Caller {
        caller(uid, PASSWD, GROUP).unwrap()
    }
    fn change(tier: Tier, files: &[&str]) -> Change {
        Change { tier, files: files.iter().map(|f| f.to_string()).collect() }
    }

    #[test]
    fn the_caller_is_the_uid() {
        assert_eq!(who(1000), Caller { uid: 1000, name: "viper".into(), agent: false, admin: true, person: true });
        assert_eq!(who(1001), Caller { uid: 1001, name: "sam".into(), agent: false, admin: false, person: true });
        assert!(!who(65534).person && !who(998).person && !who(990).person);
        assert!(who(990).agent && !who(990).admin);
        assert!(caller(4242, PASSWD, GROUP).is_err(), "no account");
        // A name that is only a prefix of a wheel member's is not one.
        assert!(!caller(1, "vipe:x:1:1::/:/bin/sh\n", GROUP).unwrap().admin);
    }

    #[test]
    fn reading_is_open_to_everyone() {
        for uid in [1000, 1001, 990] {
            for cmd in OBSERVE {
                assert_eq!(gate(&who(uid), cmd, None, None), Gate::Open, "{uid} {cmd}");
            }
        }
    }

    #[test]
    fn a_persons_own_settings_are_theirs() {
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        // No root shell over the socket, for anyone: break-glass is sudo's, with who sudo says it is.
        for uid in [1000, 1001] {
            assert!(matches!(gate(&who(uid), "break-glass", None, None), Gate::Refused(_)), "{uid}");
            // A person without the group is refused; an administrator gives their password.
            for cmd in ["snapshot-take", "snapshot-delete", "restore-data"] {
                assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
                assert!(matches!(gate(&who(1001), cmd, None, None), Gate::Refused(_)), "{cmd}");
            }
        }
        assert_eq!(gate(&who(1001), "apply", Some(&mine), None), Gate::Open);
        assert_eq!(gate(&who(1001), "revert", Some(&mine), None), Gate::Open);
        // Someone else's are not: an administrator, with their password; anyone else, refused (6a).
        let theirs = change(Tier::User, &["users/viper/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply", Some(&theirs), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "apply", Some(&mine), None), Gate::Password);
        // Their own and someone else's at once: someone else's.
        let both = change(Tier::User, &["users/sam/packages.json", "users/viper/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply", Some(&both), None), Gate::Refused(_)));
    }

    #[test]
    fn a_system_change_asks_an_administrators_password() {
        let sys = change(Tier::System, &["nixos/services.json"]);
        assert_eq!(gate(&who(1000), "apply", Some(&sys), None), Gate::Password);
        assert!(matches!(gate(&who(1001), "apply", Some(&sys), None), Gate::Refused(_)));
        for cmd in ["confirm", "reconcile", "repair"] {
            assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
            assert!(matches!(gate(&who(1001), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
        assert!(matches!(gate(&who(1000), "apply", Some(&change(Tier::Forbidden, &[".git/config"])), None), Gate::Refused(_)));
        assert!(matches!(gate(&who(1000), "apply", None, None), Gate::Refused(_)), "no such change");
    }

    #[test]
    fn a_password_is_the_persons_own_or_an_administrators() {
        assert_eq!(gate(&who(1001), "password", None, Some("sam")), Gate::Password);
        assert!(matches!(gate(&who(1001), "password", None, Some("viper")), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "password", None, Some("sam")), Gate::Password);
    }

    #[test]
    fn the_agent_proposes_and_nothing_more() {
        let a = who(990);
        assert_eq!(gate(&a, "propose", None, None), Gate::Open);
        let own = change(Tier::User, &["users/torq-agent/packages.json"]);
        for cmd in ["apply", "revert", "confirm", "reconcile", "repair", "password", "init", "setup", "expire-confirm", "serve"] {
            assert!(matches!(gate(&a, cmd, Some(&own), Some(AGENT)), Gate::Refused(_)), "{cmd}");
        }
    }

    #[test]
    fn the_machines_own_commands_are_not_on_the_socket() {
        for cmd in ["init", "setup", "expire-confirm", "serve", "push", "nonsense"] {
            assert!(matches!(gate(&who(1000), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
    }

    #[test]
    fn no_caller_names_itself_a_file_or_a_setting() {
        for bad in ["--actor", "--actor=agent", "--approved-by=viper", "--diff", "--hash-file=/etc/shadow", "--repo=/tmp/x", "--rebuild-cmd", "--state-dir"] {
            assert!(check_args(&["apply".into(), "3".into(), bad.into()]).is_err(), "{bad}");
        }
        assert!(check_args(&["--json".into(), "apply".into(), "3".into(), "--confirm".into(), "5".into(), "--confirm-tainted".into()]).is_ok());
        // A value that only looks like a flag is a value: `--reason=--actor` names nothing.
        assert!(check_args(&["propose".into(), "--reason=--actor x".into()]).is_ok());
    }

    #[test]
    fn identity_is_added_not_taken() {
        let a = with_identity(&["apply".into(), "3".into()], "apply", &who(1001), None, None, None).unwrap();
        assert_eq!(a, ["apply", "3", "--approved-by", "sam"]);
        assert!(with_identity(&["propose".into(), "--reason".into(), "x".into()], "propose", &who(1000), Some("/run/d"), None, None).is_err(), "no base");
        let p = with_identity(&["propose".into(), "--base=c".into(), "--reason".into(), "x".into()], "propose", &who(990), Some("/run/d"), None, None).unwrap();
        assert_eq!(p, ["propose", "--base=c", "--reason", "x", "--actor", "agent", "--diff", "/run/d"]);
        assert!(with_identity(&["propose".into()], "propose", &who(1000), None, None, None).is_err(), "a proposal brings its diff");
    }

    /// The audit of ADR-040 (2026-11-04, its 2): `apply --confirm` holds every other change back
    /// until it is confirmed or expires — an administrator's to do, own folder or not.
    #[test]
    fn holding_the_machine_is_an_administrators() {
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        assert!(matches!(gate(&who(1001), "apply-confirmed", Some(&mine), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1000), "apply-confirmed", Some(&mine), None), Gate::Password);
        assert!(matches!(gate(&who(990), "apply-confirmed", Some(&mine), None), Gate::Refused(_)));
    }

    /// `commit at`: approved now, as an apply is (a person's own settings theirs, a system change an
    /// administrator's with their password); cancelled by an administrator; run only by its timer.
    #[test]
    fn a_commit_at_is_approved_as_an_apply_is() {
        let sys = change(Tier::System, &["nixos/services.json"]);
        let mine = change(Tier::User, &["users/sam/packages.json"]);
        assert_eq!(gate(&who(1000), "schedule", Some(&sys), None), Gate::Password);
        assert!(matches!(gate(&who(1001), "schedule", Some(&sys), None), Gate::Refused(_)));
        assert_eq!(gate(&who(1001), "schedule", Some(&mine), None), Gate::Open);
        assert!(matches!(gate(&who(1000), "schedule", None, None), Gate::Refused(_)), "no such proposal");
        assert!(matches!(gate(&who(1000), "schedule", Some(&change(Tier::Forbidden, &[".git/config"])), None), Gate::Refused(_)));
        assert!(matches!(gate(&who(990), "schedule", Some(&mine), None), Gate::Refused(_)), "the agent");
        assert_eq!(gate(&who(1000), "cancel-scheduled", None, None), Gate::Password);
        assert!(matches!(gate(&who(1001), "cancel-scheduled", None, None), Gate::Refused(_)));
        // Its approver cancels their own without a password (`user`: who approved it, as the ledger says).
        assert_eq!(gate(&who(1001), "cancel-scheduled", None, Some("sam")), Gate::Open);
        assert!(matches!(gate(&who(1001), "cancel-scheduled", None, Some("viper")), Gate::Refused(_)));
        assert!(matches!(gate(&who(990), "cancel-scheduled", None, Some(AGENT)), Gate::Refused(_)), "the agent cancels nothing");
        assert_eq!(account("sam", PASSWD, GROUP).unwrap(), who(1001));
        assert!(account("nobody-here", PASSWD, GROUP).is_err());
        assert!(account("sam", "sam:x:1000:100::/:/bin/sh\nviper:x:1000:100::/:/bin/sh\n", GROUP).is_ok());
        assert!(account("viper", "sam:x:1000:100::/:/bin/sh\nviper:x:1000:100::/:/bin/sh\n", GROUP).is_err(), "a uid shared is not one account");
        for uid in [1000, 1001, 990] {
            assert!(matches!(gate(&who(uid), "run-scheduled", None, None), Gate::Refused(_)), "{uid}");
            assert!(matches!(gate(&who(uid), "resume-scheduled", None, None), Gate::Refused(_)), "{uid}");
            assert_eq!(gate(&who(uid), "scheduled", None, None), Gate::Open, "{uid}");
        }
        let a = with_identity(&["schedule".into(), "3".into(), "--at=x".into()], "schedule", &who(1001), None, None, None).unwrap();
        assert_eq!(a, ["schedule", "3", "--at=x", "--approved-by", "sam"]);
        assert!(check_args(&["schedule".into(), "--commit-at-cmd=true".into()]).is_err());
        assert!(check_args(&["schedule".into(), "--commit-at-cancel-cmd".into()]).is_err());
    }

    /// ADR-042 (amended): the agent's sessions are the agent's; trust in it is a person's, given with
    /// their own password, for their own folder (the approver, as the kernel says), ended without one.
    #[test]
    fn sessions_are_the_agents_and_trust_a_persons() {
        let a = who(990);
        for cmd in ["session-start", "session-taint"] {
            assert_eq!(gate(&a, cmd, None, None), Gate::Open, "{cmd}");
            for uid in [1000, 1001, 65534] {
                assert!(matches!(gate(&who(uid), cmd, None, None), Gate::Refused(_)), "{cmd} {uid}");
            }
        }
        for cmd in ["trust-grant", "trust-revoke"] {
            assert!(matches!(gate(&a, cmd, None, None), Gate::Refused(_)), "the agent trusts itself with nothing: {cmd}");
            assert!(matches!(gate(&who(65534), cmd, None, None), Gate::Refused(_)), "{cmd}");
        }
        // Not only an administrator's: any person's, for their own folder, with their password.
        assert_eq!(gate(&who(1001), "trust-grant", None, None), Gate::Password);
        assert_eq!(gate(&who(1000), "trust-grant", None, None), Gate::Password);
        assert_eq!(gate(&who(1001), "trust-revoke", None, None), Gate::Open);
        for uid in [1000, 1001, 990] {
            assert_eq!(gate(&who(uid), "trust", None, None), Gate::Open);
            assert_eq!(gate(&who(uid), "session", None, None), Gate::Open);
        }
        // Whose trust it is, is the kernel's answer, never the caller's.
        let g = with_identity(&["trust-grant".into(), "--minutes=30".into()], "trust-grant", &who(1001), None, None, None).unwrap();
        assert_eq!(g, ["trust-grant", "--minutes=30", "--approved-by", "sam"]);
        assert!(check_args(&["trust-grant".into(), "--minutes=30".into(), "--approved-by=viper".into()]).is_err());
        let r = with_identity(&["trust-revoke".into()], "trust-revoke", &who(1000), None, None, None).unwrap();
        assert_eq!(r, ["trust-revoke", "--approved-by", "viper"]);
    }

    /// The review of pull: a fetch goes to the network, as root — a person's, not everyone's.
    #[test]
    fn only_people_fetch_from_the_remote() {
        assert_eq!(gate(&who(1001), "fetch-incoming", None, None), Gate::Open);
        assert!(matches!(gate(&who(990), "fetch-incoming", None, None), Gate::Refused(_)), "the agent");
        assert!(matches!(gate(&who(65534), "fetch-incoming", None, None), Gate::Refused(_)), "nobody");
    }

    /// Its 5: a service's account (nobody, sshd) proposes nothing — every proposal stays in the
    /// ledger for good; reading is still everyone's.
    #[test]
    fn services_propose_nothing() {
        for uid in [65534, 998] {
            assert!(matches!(gate(&who(uid), "propose", None, None), Gate::Refused(_)), "{uid}");
            assert_eq!(gate(&who(uid), "log", None, None), Gate::Open, "{uid}");
        }
    }

    /// ADR-038: a backup's key and a restore from it are an administrator's, with their password; the
    /// list is everyone's. The key comes in the request, never as a path, and never from the agent.
    #[test]
    fn a_backup_is_an_administrators() {
        for cmd in ["backup-key", "backup-restore"] {
            assert_eq!(gate(&who(1000), cmd, None, None), Gate::Password, "{cmd}");
            for uid in [1001, 990, 65534] {
                assert!(matches!(gate(&who(uid), cmd, None, None), Gate::Refused(_)), "{cmd} {uid}");
            }
        }
        for uid in [1000, 1001, 990] {
            assert_eq!(gate(&who(uid), "backups", None, None), Gate::Open, "{uid}");
        }
        assert!(check_args(&["backup-key".into(), "--name=home".into(), "--key-file=/etc/shadow".into()]).is_err());
        let k = with_identity(&["backup-key".into(), "--name".into(), "home".into()], "backup-key", &who(1000), None, None, Some("/run/k")).unwrap();
        assert_eq!(k, ["backup-key", "--name", "home", "--key-file", "/run/k", "--approved-by", "viper"]);
        assert!(with_identity(&["backup-key".into()], "backup-key", &who(1000), None, None, None).is_err(), "a key comes in the request");
        let r = with_identity(&["backup-restore".into(), "--name=home".into(), "--path=/home/sam".into()], "backup-restore", &who(1000), None, None, None).unwrap();
        assert_eq!(r.last().map(String::as_str), Some("viper"));
    }
}
