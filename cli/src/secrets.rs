//! torqcli's side of the secrets (ADR-052): the rules, changed as commits of their files, and the list.
//! The values never pass through here except to the broker (`request system secret set`, typed at the
//! terminal) and back to the person who asked (`show`): the Machine does that part (shell.rs).
//! TorqOS Settings uses the same functions.

use crate::candidate::Repo;
use crate::session::Machine;
use anyhow::{bail, Context, Result};
use torq_wire::secrets::{self as w, Approve, Https, Kind, Rule, Rules, Ssh};

/// What a rule lets happen, given or taken back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Grant {
    /// The agent (torq-agent) may use it.
    Agent,
    /// Sent in an HTTPS header to this host.
    Https(String),
    /// Signs for SSH to this host.
    Ssh(String),
}

/// `request system secret <act> <name> … [user <person>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Act {
    Set,
    /// The value from a file (an SSH key: several lines, not typed).
    SetFile(String),
    Delete,
    Show,
    Allow(Grant),
    Disallow(Grant),
    Approve(Approve),
}

impl Act {
    pub fn verb(&self) -> &'static str {
        match self {
            Act::Set | Act::SetFile(_) => "set",
            Act::Delete => "delete",
            Act::Show => "show",
            Act::Allow(_) => "allow",
            Act::Disallow(_) => "disallow",
            Act::Approve(_) => "approve",
        }
    }

    /// The words after the secret's name.
    pub fn rest(&self) -> String {
        let grant = |g: &Grant| match g {
            Grant::Agent => " agent".to_string(),
            Grant::Https(h) => format!(" https {h}"),
            Grant::Ssh(h) => format!(" ssh {h}"),
        };
        match self {
            Act::Allow(g) | Act::Disallow(g) => grant(g),
            Act::Approve(a) => format!(" {}", a.word()),
            Act::SetFile(p) => format!(" file {}", crate::syntax::quote(p)),
            _ => String::new(),
        }
    }
}

/// Where `user`'s rules are in the repository (None: the system's).
pub fn rules_path(user: Option<&str>) -> String {
    match user {
        None => w::SYSTEM_RULES.to_string(),
        Some(u) => format!("users/{u}/{}", w::PERSON_RULES),
    }
}

fn whose(name: &str, user: Option<&str>) -> String {
    match user {
        None => format!("the system's secret {name}"),
        Some(u) => format!("{u}'s secret {name}"),
    }
}

/// `user`'s rules at `rev`: none yet is none at all; a file that is not a rules file is said, not taken.
pub fn read(repo: &Repo, rev: &str, user: Option<&str>) -> Result<Rules> {
    let path = rules_path(user);
    match repo.file_at(rev, &path)? {
        None => Ok(Rules::new()),
        Some(bytes) => if user.is_some() { w::parse_person(&bytes) } else { w::parse(&bytes) }.map_err(|why| anyhow::anyhow!("{path}: {why}")),
    }
}

/// `act` done to `rules`: what it did, in a few words, for the change's reason. Refused, it changes nothing.
pub fn edit(rules: &mut Rules, name: &str, act: &Act) -> Result<String> {
    edit_with(rules, name, act, &[])
}

/// `edit`, with the keys `known` of the host an `allow … ssh <host>` names (known_hosts lines, from the person's
/// own known_hosts): what OpenSSH's agent checks that host against before it signs.
pub fn edit_with(rules: &mut Rules, name: &str, act: &Act, known: &[String]) -> Result<String> {
    if !w::name_ok(name) {
        bail!("'{name}' is not a secret's name (letters, digits, - and _)");
    }
    let before = rules.clone();
    let what = edit_unchecked(rules, name, act, known);
    let done = what.and_then(|what| w::check(rules).map(|()| what).map_err(|why| anyhow::anyhow!("{why}")));
    if done.is_err() {
        *rules = before;
    }
    done
}

fn edit_unchecked(rules: &mut Rules, name: &str, act: &Act, known: &[String]) -> Result<String> {
    let first = || anyhow::anyhow!("secret {name} is not used for anything yet: say what it is for first (request system secret allow {name} https <host>, or ssh <host>)");
    match act {
        Act::Allow(Grant::Https(h)) | Act::Allow(Grant::Ssh(h)) if !w::host_ok(h) => {
            bail!("'{h}' is not a host name (lowercase, with a dot; no address, no wildcard)")
        }
        Act::Allow(Grant::Https(h)) => {
            let r = rules.entry(name.into()).or_insert_with(|| Rule::new(Kind::Token));
            if r.kind != Kind::Token {
                bail!("secret {name} is an SSH key: it is never sent, so it has no https use");
            }
            let https = r.https.get_or_insert_with(|| Https { hosts: Vec::new(), header: "Authorization".into(), prefix: "Bearer ".into() });
            if https.hosts.contains(h) {
                bail!("secret {name} is already sent to {h}");
            }
            https.hosts.push(h.clone());
            https.hosts.sort();
            Ok(format!("may be sent to {h}"))
        }
        Act::Allow(Grant::Ssh(h)) => {
            let r = rules.entry(name.into()).or_insert_with(|| Rule::new(Kind::SshKey));
            if r.kind != Kind::SshKey {
                bail!("secret {name} is a token: it has no ssh use");
            }
            let ssh = r.ssh.get_or_insert_with(|| Ssh { hosts: Vec::new(), known: Vec::new() });
            if ssh.hosts.contains(h) {
                bail!("secret {name} already signs for {h}");
            }
            ssh.hosts.push(h.clone());
            // Its keys as the person knows them, for that host alone.
            ssh.known.extend(known.iter().filter(|l| l.split(' ').next() == Some(h.as_str()) && !ssh.known.contains(l)).cloned().collect::<Vec<_>>());
            ssh.hosts.sort();
            Ok(format!("may sign for {h}"))
        }
        Act::Allow(Grant::Agent) => {
            let r = rules.get_mut(name).ok_or_else(first)?;
            if r.actors.iter().any(|a| a == w::AGENT) {
                bail!("the agent may already use secret {name}");
            }
            r.actors.push(w::AGENT.into());
            r.actors.sort();
            Ok("the agent may use it".into())
        }
        Act::Disallow(g) => {
            let r = rules.get_mut(name).with_context(|| format!("secret {name} has no rule"))?;
            let (list, what) = match g {
                Grant::Agent => (&mut r.actors, w::AGENT.to_string()),
                Grant::Https(h) => (r.https.as_mut().map(|x| &mut x.hosts).with_context(|| format!("secret {name} is sent nowhere"))?, h.clone()),
                Grant::Ssh(h) => (r.ssh.as_mut().map(|x| &mut x.hosts).with_context(|| format!("secret {name} signs for nothing"))?, h.clone()),
            };
            if !list.contains(&what) {
                bail!("{what} is not allowed it now");
            }
            list.retain(|x| x != &what);
            // A host taken back takes its keys with it.
            if let (Grant::Ssh(h), Some(s)) = (g, r.ssh.as_mut()) {
                s.known.retain(|l| l.split(' ').next() != Some(h.as_str()));
            }
            // A use with no host left is no use.
            if r.https.as_ref().is_some_and(|x| x.hosts.is_empty()) {
                r.https = None;
            }
            if r.ssh.as_ref().is_some_and(|x| x.hosts.is_empty()) {
                r.ssh = None;
            }
            Ok(match g {
                Grant::Agent => "the agent may no longer use it".into(),
                Grant::Https(h) => format!("no longer sent to {h}"),
                Grant::Ssh(h) => format!("no longer signs for {h}"),
            })
        }
        Act::Approve(a) => {
            let r = rules.get_mut(name).ok_or_else(first)?;
            r.approve = *a;
            Ok(match a {
                Approve::EachUse => "a person approves each use by the agent".into(),
                Approve::FirstUse => "a person approves the agent's first use in each session".into(),
                Approve::Never => "the agent uses it without asking (a tainted session still asks)".into(),
            })
        }
        Act::Delete => {
            rules.remove(name).context("no rule")?;
            Ok("its rule removed".into())
        }
        Act::Set | Act::SetFile(_) | Act::Show => bail!("not a change of the rules"),
    }
}

/// `act` on `user`'s rules, committed: no question for a person's own, an administrator's approval for the
/// system's (the broker asks). The change's number.
pub fn change(machine: &dyn Machine, repo: &Repo, name: &str, user: Option<&str>, act: &Act) -> Result<u64> {
    let head = repo.head()?;
    let path = rules_path(user);
    let old = repo.file_at(&head, &path)?;
    let mut rules = read(repo, &head, user)?;
    // An SSH host's keys, as the person asking knows them (their own known_hosts): never taken on first sight.
    let known = match act {
        Act::Allow(Grant::Ssh(h)) => machine.known_host_keys(h)?,
        _ => Vec::new(),
    };
    let what = edit_with(&mut rules, name, act, &known)?;
    let new = (!rules.is_empty()).then(|| w::render(&rules));
    let scratch = crate::scratch_dir()?;
    let diff = crate::candidate::unified_diff_bytes(scratch.path(), &[(path, old.as_deref(), new.as_deref().map(str::as_bytes))])?;
    let tier = if user.is_some() { "user" } else { "system" };
    let mut reason = format!("Secret {name}: {what}");
    if user.is_none() {
        reason = format!("The system's secret {name}: {what}");
    }
    machine.commit(&diff, &head, tier, &reason, None)
}

/// `request system secret <act> …` for the rules: done, and said.
pub fn changed(machine: &dyn Machine, repo: &Repo, name: &str, user: Option<&str>, act: &Act) -> Result<String> {
    let seq = change(machine, repo, name, user, act)?;
    Ok(format!("Changed what {} may be used for (change #{seq}).\n", whose(name, user)))
}

/// The largest value the broker keeps (broker/src/secrets.rs): a file larger is not read.
pub const VALUE_MAX: usize = 16 * 1024;

/// A secret that has a value, as the broker lists them (None: the system's).
pub type Kept = (Option<String>, String);

/// Every person with a rules file at `rev`.
pub fn people_with_rules(repo: &Repo, rev: &str) -> Result<Vec<String>> {
    let mut people: Vec<String> = repo
        .paths(rev)?
        .into_iter()
        .filter_map(|p| p.strip_prefix("users/").and_then(|r| r.strip_suffix(&format!("/{}", w::PERSON_RULES))).map(String::from))
        .filter(|u| !u.contains('/'))
        .collect();
    people.sort();
    Ok(people)
}

/// `show system secrets`: each secret, whose, whether it has a value, and what it may be used for.
pub fn shown(repo: &Repo, kept: &[Kept]) -> Result<String> {
    let head = repo.head()?;
    let mut owners: Vec<Option<String>> = vec![None];
    let mut people = people_with_rules(repo, &head)?;
    people.extend(kept.iter().filter_map(|(u, _)| u.clone()));
    people.sort();
    people.dedup();
    owners.extend(people.into_iter().map(Some));
    let mut out = String::new();
    for owner in owners {
        let rules = match read(repo, &head, owner.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                out.push_str(&format!("! {e:#}\n"));
                Rules::new()
            }
        };
        let mut names: Vec<&String> = rules.keys().collect();
        let valued: Vec<&String> = kept.iter().filter(|(u, _)| *u == owner).map(|(_, n)| n).collect();
        names.extend(valued.iter().copied());
        names.sort();
        names.dedup();
        if names.is_empty() {
            continue;
        }
        out.push_str(&match &owner {
            None => "The system's secrets:\n".to_string(),
            Some(u) => format!("{u}'s secrets:\n"),
        });
        for n in names {
            let value = if valued.contains(&n) { "has a value" } else { "no value yet" };
            let Some(r) = rules.get(n) else {
                out.push_str(&format!("  {n:<20} {value}; used for nothing\n"));
                continue;
            };
            let kind = match r.kind {
                Kind::Token => "token",
                Kind::SshKey => "SSH key",
            };
            let mut uses = Vec::new();
            if let Some(h) = &r.https {
                uses.push(format!("https to {}", h.hosts.join(", ")));
            }
            if let Some(s) = &r.ssh {
                uses.push(format!("ssh to {}", s.hosts.join(", ")));
            }
            let who = if r.actors.iter().any(|a| a == w::AGENT) { format!("the agent too, approved {}", r.approve.word()) } else { "not the agent".to_string() };
            let uses = if uses.is_empty() { "used for nothing".to_string() } else { uses.join("; ") };
            out.push_str(&format!("  {n:<20} {kind}, {value}; {uses}; {who}\n"));
            // Where a program sends its request for each host (ADR-052): the daemon puts the secret in.
            for h in r.https.iter().flat_map(|x| x.hosts.iter()) {
                let whose = owner.as_ref().map_or_else(|| "system".to_string(), |u| format!("user/{u}"));
                out.push_str(&format!("  {:<20} use: http://127.0.0.1:{}/{whose}/{n}/{h}/\n", "", w::PORT));
            }
        }
    }
    if out.is_empty() {
        out.push_str("No secrets yet: request system secret set <name> keeps one.\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_said_what_it_is_for_then_who() {
        let mut r = Rules::new();
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Agent)).is_err(), "what it is for first");
        assert!(edit(&mut r, "gh", &Act::Approve(Approve::Never)).is_err());
        assert_eq!(edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).unwrap(), "may be sent to api.github.com");
        edit(&mut r, "gh", &Act::Allow(Grant::Agent)).unwrap();
        edit(&mut r, "gh", &Act::Approve(Approve::EachUse)).unwrap();
        let g = &r["gh"];
        assert_eq!((g.kind, g.actors.clone(), g.approve), (Kind::Token, vec![w::AGENT.to_string()], Approve::EachUse));
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Agent)).is_err(), "already");
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Ssh("github.com".into()))).is_err(), "a token signs for nothing");
        assert!(edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).is_err(), "already");
    }

    #[test]
    fn what_is_refused_changes_nothing() {
        let mut r = Rules::new();
        edit_with(&mut r, "k", &Act::Allow(Grant::Ssh("github.com".into())), &["github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl".into()]).unwrap();
        let before = r.clone();
        for (name, act) in [
            ("k", Act::Allow(Grant::Https("api.github.com".into()))),
            ("k", Act::Allow(Grant::Ssh("10.0.0.1".into()))),
            ("k", Act::Allow(Grant::Ssh("*.github.com".into()))),
            ("k", Act::Disallow(Grant::Ssh("gitlab.com".into()))),
            ("k", Act::Disallow(Grant::Agent)),
            ("../k", Act::Allow(Grant::Ssh("github.com".into()))),
            ("none", Act::Delete),
        ] {
            assert!(edit(&mut r, name, &act).is_err(), "{name} {act:?}");
            assert_eq!(r, before, "{name} {act:?}");
        }
    }

    #[test]
    fn taken_back_a_use_with_no_host_is_no_use() {
        let mut r = Rules::new();
        edit(&mut r, "gh", &Act::Allow(Grant::Https("api.github.com".into()))).unwrap();
        edit(&mut r, "gh", &Act::Allow(Grant::Https("uploads.github.com".into()))).unwrap();
        edit(&mut r, "gh", &Act::Allow(Grant::Agent)).unwrap();
        edit(&mut r, "gh", &Act::Disallow(Grant::Https("api.github.com".into()))).unwrap();
        assert_eq!(r["gh"].https.as_ref().unwrap().hosts, ["uploads.github.com"]);
        edit(&mut r, "gh", &Act::Disallow(Grant::Https("uploads.github.com".into()))).unwrap();
        assert_eq!(r["gh"].https, None);
        edit(&mut r, "gh", &Act::Disallow(Grant::Agent)).unwrap();
        assert!(r["gh"].actors.is_empty());
        edit(&mut r, "gh", &Act::Delete).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn the_words_read_back() {
        assert_eq!(Act::Allow(Grant::Https("a.com".into())).rest(), " https a.com");
        assert_eq!(Act::Disallow(Grant::Agent).rest(), " agent");
        assert_eq!(Act::Approve(Approve::FirstUse).rest(), " first-use");
        assert_eq!(rules_path(None), "secrets/system.json");
        assert_eq!(rules_path(Some("casey")), "users/casey/secrets.json");
    }
}
