//! `torq agent run [--share <dir>] [--] [<command> …]` (ADR-041, 5): a program — Claude Code, or any
//! other — run as the agent's account, not as the person who starts it.
//!
//! It runs as `torq-agent` (through `sudo torq-agent-run`, which binds the shared directory as root and
//! then drops to the agent for good), in
//! the launcher's sandbox (modules/agent-run.sh): every home hidden but the one directory shared with
//! it, the people's runtime directories hidden, /tmp its own. Its home is the agent's, so a tool's
//! settings and credentials are never the person's. What it changes in the system, it proposes —
//! torqcli as the agent stops at the proposal — and a person applies (`request system apply`).
//!
//! Sharing a directory is the person's act, and visible: an ACL on what of it is theirs (and default
//! ACLs, so that files either of them makes stay both of theirs). The agent gets no way into their
//! home: torqcli opens the directory, as the person, and the launcher binds that open descriptor —
//! everything around it stays out of the agent's reach, by name or otherwise.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENT: &str = "torq-agent";
const SUDO: &str = "/run/wrappers/bin/sudo";
const LAUNCHER: &str = "/run/current-system/sw/bin/torq-agent-run";
const SETFACL: &str = "/run/current-system/sw/bin/setfacl";

/// What `torq agent run` was asked: the directory to share, and the program (none: a shell).
#[derive(Debug, PartialEq)]
pub struct Run {
    pub share: Option<PathBuf>,
    pub command: Vec<String>,
}

/// `run [--share <dir>] [--] [<command> …]`, after `agent`.
pub fn parse(args: &[String]) -> Result<Run> {
    let usage = "usage: torq agent run [--share <dir>] [--] [<command> …]";
    let mut it = args.iter();
    if it.next().map(String::as_str) != Some("run") {
        bail!("{usage}");
    }
    let rest: Vec<&String> = it.collect();
    let (share, rest) = match rest.as_slice() {
        [flag, dir, rest @ ..] if flag.as_str() == "--share" => (Some(PathBuf::from(dir.as_str())), rest),
        [flag] if flag.as_str() == "--share" => bail!("--share needs a directory\n{usage}"),
        rest => (None, rest),
    };
    let (dashes, rest) = match rest {
        [d, rest @ ..] if d.as_str() == "--" => (true, rest),
        rest => (false, rest),
    };
    if !dashes && rest.first().is_some_and(|a| a.starts_with('-')) {
        bail!("unknown option {}\n{usage}", rest[0]);
    }
    Ok(Run { share, command: rest.iter().map(|s| s.to_string()).collect() })
}

/// The directory to share, checked: it exists, it is a directory, it is the person's own, and it is
/// inside their home but not the home itself — named as it is, every link resolved.
pub fn checked_share(dir: &Path, home: &Path, uid: u32) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let dir = std::fs::canonicalize(dir).with_context(|| format!("{}: no such directory", dir.display()))?;
    let home = std::fs::canonicalize(home).with_context(|| format!("{}: your home", home.display()))?;
    let m = std::fs::metadata(&dir)?;
    if !m.is_dir() {
        bail!("{}: not a directory", dir.display());
    }
    if dir == home || !dir.starts_with(&home) {
        bail!("{}: share a directory inside your home ({}), not the home itself or anything outside it", dir.display(), home.display());
    }
    if m.uid() != uid {
        bail!("{}: not yours to share", dir.display());
    }
    Ok(dir)
}


/// Everything under `dir` (itself included) that `uid` owns — its directories, and its other files —
/// walked without following a link (a link is not shared; what it points to is not walked).
pub fn owned(dir: &Path, uid: u32) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    use std::os::unix::fs::MetadataExt;
    let (mut dirs, mut files, mut todo) = (Vec::new(), Vec::new(), vec![dir.to_path_buf()]);
    while let Some(p) = todo.pop() {
        let m = std::fs::symlink_metadata(&p).with_context(|| format!("reading {}", p.display()))?;
        if m.file_type().is_symlink() || m.uid() != uid {
            continue;
        }
        if m.is_dir() {
            for e in std::fs::read_dir(&p).with_context(|| format!("reading {}", p.display()))? {
                todo.push(e?.path());
            }
            dirs.push(p);
        } else if m.is_file() {
            files.push(p);
        }
    }
    Ok((dirs, files))
}


/// A process of the agent's account running now, if there is one (its pid): /proc's entries are
/// owned by each process's user.
fn agent_running() -> Result<Option<u32>> {
    use std::os::unix::fs::MetadataExt;
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    let Some(uid) = passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.first() == Some(&AGENT)).then(|| f.get(2)?.parse::<u32>().ok()).flatten()
    }) else {
        bail!("there is no {AGENT} account on this machine");
    };
    for e in std::fs::read_dir("/proc").context("reading /proc")?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        if e.metadata().is_ok_and(|m| m.uid() == uid) {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}
/// setfacl with `args` on each of `paths` at once.
fn acl_many(args: &[&str], paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if !Command::new(SETFACL).args(args).arg("--").args(paths).status().context("running setfacl")?.success() {
        bail!("could not share {} (setfacl)", paths[0].display());
    }
    Ok(())
}
/// Run it: share the directory (ACLs, and the directory itself, open), then become the agent,
/// through the launcher. Does not return on success: the program takes this process's place.
pub fn run(args: &[String]) -> Result<i32> {
    use std::os::fd::AsRawFd;
    let r = parse(args)?;
    let me = crate::me()?;
    if me == AGENT {
        bail!("this is the agent already");
    }
    // The launcher by its own path in the store, as the sudo rule names it (sudo does not take a link
    // to it for it).
    let launcher = std::fs::canonicalize(LAUNCHER).with_context(|| format!("{LAUNCHER}: the agent's launcher is not installed"))?;
    let launcher = launcher.to_string_lossy().into_owned();
    let mut sudo: Vec<String> = Vec::new();
    let mut launch = vec![launcher];
    // Held open until the program has taken this process's place: the launcher binds it.
    let mut _shared: Option<std::fs::File> = None;
    if let Some(share) = &r.share {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set: no home to share from")?);
        let uid = rustix::process::getuid().as_raw();
        let dir = checked_share(share, &home, uid)?;
        // One session of the agent's at a time, and none while the directory is walked: a program of the
        // agent's running now could swap a file it can write for a link, between the walk's look and its
        // ACL, and be granted wherever the link leads (the review of ADR-041, 5). With its own process
        // namespace (the launcher), nothing of a session outlives it.
        if let Some(pid) = agent_running()? {
            bail!("the agent is running already (process {pid}): one session at a time, and none while a directory is shared");
        }
        // The agent may read and write what of it is the person's; what it makes there, the person may
        // rename and delete, and (default ACLs) read and write unless the agent narrows it — the agent
        // owns what it makes. What the agent made is not the person's to set ACLs on. No link is followed.
        let (dirs, files) = owned(&dir, uid)?;
        for chunk in files.chunks(200) {
            acl_many(&["-m", &format!("u:{AGENT}:rwX")], chunk)?;
        }
        for chunk in dirs.chunks(200) {
            acl_many(&["-m", &format!("u:{AGENT}:rwx")], chunk)?;
            acl_many(&["-d", "-m", &format!("u:{AGENT}:rwX,u:{me}:rwX")], chunk)?;
        }
        // Opened as the person, kept open across exec (no close-on-exec), and kept by sudo (`-C`,
        // which the launcher's rule allows): the agent is handed the directory, not a way to it.
        let f = std::fs::File::open(&dir).with_context(|| format!("opening {}", dir.display()))?;
        rustix::io::fcntl_setfd(&f, rustix::io::FdFlags::empty()).context("keeping the shared directory open")?;
        let fd = f.as_raw_fd();
        sudo.extend(["-C".into(), (fd + 1).to_string()]);
        launch.extend(["--share-fd".into(), fd.to_string(), dir.to_string_lossy().into_owned()]);
        _shared = Some(f);
        eprintln!("Shared with the agent: {} (undo: setfacl -R -P -x u:{AGENT},d:u:{AGENT} -- {})", dir.display(), dir.display());
    }
    launch.push("--".into());
    launch.extend(r.command);
    use std::os::unix::process::CommandExt;
    let e = Command::new(SUDO).args(&sudo).arg("--").args(&launch).exec();
    Err(anyhow::Error::from(e).context("starting the agent (sudo torq-agent-run)"))
}

/// `torq break-glass "<reason>"` (ADR-035): a root shell, after the record. The launcher, through
/// sudo (the person's password), writes the ledger entry, pushes it, and only then opens the shell.
pub fn break_glass(args: &[String]) -> Result<i32> {
    const LAUNCHER: &str = "/run/current-system/sw/bin/torq-break-glass";
    let reason = args.join(" ");
    if reason.trim().is_empty() {
        bail!("usage: torq break-glass \"<why>\" — the reason goes in the ledger, before the shell opens");
    }
    if crate::me()? == AGENT {
        bail!("the agent has no root shell: it proposes, and a person applies");
    }
    let launcher = std::fs::canonicalize(LAUNCHER).with_context(|| format!("{LAUNCHER}: not on this machine"))?;
    use std::os::unix::process::CommandExt;
    let e = Command::new(SUDO).arg("--").arg(launcher).arg(&reason).exec();
    Err(anyhow::Error::from(e).context("starting break-glass (sudo)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn what_it_is_asked() {
        assert_eq!(parse(&a("run")).unwrap(), Run { share: None, command: vec![] });
        assert_eq!(parse(&a("run claude")).unwrap(), Run { share: None, command: a("claude") });
        assert_eq!(parse(&a("run --share /home/v/p -- claude --resume")).unwrap(), Run { share: Some("/home/v/p".into()), command: a("claude --resume") });
        assert_eq!(parse(&a("run -- -x")).unwrap().command, a("-x"), "after --, anything");
        assert!(parse(&a("run --share")).is_err());
        assert!(parse(&a("run --shar /x")).is_err(), "no other option");
        assert!(parse(&a("start")).is_err());
    }

    #[test]
    fn only_a_directory_of_ones_own_inside_ones_home() {
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let project = home.join("src/project");
        std::fs::create_dir_all(&project).unwrap();
        let uid = std::fs::metadata(&home).unwrap().uid();
        assert_eq!(checked_share(&project, &home, uid).unwrap(), std::fs::canonicalize(&project).unwrap());
        assert!(checked_share(&home, &home, uid).is_err(), "not the home itself");
        assert!(checked_share(d.path(), &home, uid).is_err(), "not outside it");
        assert!(checked_share(&project, &home, uid + 1).is_err(), "not someone else's");
        std::fs::write(home.join("file"), "").unwrap();
        assert!(checked_share(&home.join("file"), &home, uid).is_err(), "not a file");
        // A link out of the home is where it leads, and refused there.
        std::os::unix::fs::symlink(d.path(), home.join("out")).unwrap();
        assert!(checked_share(&home.join("out"), &home, uid).is_err());
    }

    #[test]
    fn the_walk_takes_only_ones_own_and_follows_no_link() {
        use std::os::unix::fs::MetadataExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("p");
        std::fs::create_dir_all(p.join("sub")).unwrap();
        std::fs::write(p.join("sub/f"), "").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), p.join("link")).unwrap();
        let uid = std::fs::metadata(&p).unwrap().uid();
        let (dirs, files) = owned(&p, uid).unwrap();
        assert_eq!(dirs.len(), 2, "{dirs:?}");
        assert_eq!(files, [p.join("sub/f")]);
        assert_eq!(owned(&p, uid + 1).unwrap(), (vec![], vec![]), "someone else's: nothing");
    }

}
