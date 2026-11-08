//! `torq-doctor [--propose] "<question>"` (ADR-042): TorqOS's agent, run as `torq-agent` (by `torq
//! doctor` and `torq agent propose`, through `torq agent run`). It reads — the journal, the hardware,
//! the configuration, the ledger and alarms, NixOS's option docs — and explains; it starts its session
//! at the broker and reports there each read of untrusted text. In propose mode it may also propose a
//! change, through torqcli as the agent, which stops at the proposal: it never applies one.

// No unsafe code and no dead code, ever (ADR-020); in a test build `deny`, as torqcli's (cli/src/main.rs).
#![forbid(unsafe_code)]
#![cfg_attr(not(test), forbid(dead_code))]
#![cfg_attr(test, deny(dead_code))]

mod clean;
mod doctor;
mod model;
mod session;
mod tools;

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const SETTINGS: &str = "/etc/torq/agent.json";

/// The machine: a program run with nothing of this process's environment but a fixed PATH, its
/// home and plain output, its input closed, its output and its time bounded (tools::bounded).
struct Machine {
    home: PathBuf,
}

impl Machine {
    fn command(&self, program: &Path, args: &[String]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env_clear()
            .env("PATH", "/run/current-system/sw/bin")
            .env("HOME", &self.home)
            .env("NO_COLOR", "1")
            .env("TERM", "dumb")
            .env("LANG", "C.UTF-8");
        cmd
    }
}

impl tools::Host for Machine {
    fn run(&self, program: &Path, args: &[String]) -> Result<String> {
        tools::bounded(self.command(program, args), tools::MAX_PROGRAM_BYTES, tools::PROGRAM_TIMEOUT).with_context(|| format!("running {}", program.display()))
    }

    fn propose(&self, script: &str, session: &str) -> Result<String> {
        let mut cmd = self.command(Path::new(tools::TORQCLI), &["-c".into(), script.into()]);
        cmd.env("TORQ_AGENT_SESSION", session);
        tools::bounded(cmd, tools::MAX_PROGRAM_BYTES, tools::PROPOSE_TIMEOUT).context("running torqcli to propose")
    }

    fn session(&self, args: &[&str]) -> Result<String> {
        let mut a: Vec<String> = vec!["agent".into(), "session".into()];
        a.extend(args.iter().map(|s| s.to_string()));
        let out = self.command(Path::new(tools::TORQCLI), &a).stdin(std::process::Stdio::null()).output().context("running torqcli agent session")?;
        if !out.status.success() {
            bail!("{}", clean::clean(String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("torq-doctor: {}", clean::clean(&format!("{e:#}")));
            std::process::exit(1);
        }
    }
}

/// `[--propose] [--] <question>`: whether in propose mode, and the question.
fn parse(args: &[String]) -> Result<(bool, String)> {
    let usage = "usage: torq-doctor [--propose] [--] \"<question>\" (as a person: torq doctor \"<question>\", torq agent propose \"<ask>\")";
    let (propose, rest) = match args {
        [p, rest @ ..] if p == "--propose" => (true, rest),
        rest => (false, rest),
    };
    let (dashes, rest) = match rest {
        [d, rest @ ..] if d == "--" => (true, rest),
        rest => (false, rest),
    };
    let question = rest.join(" ");
    if question.trim().is_empty() || (!dashes && rest.first().is_some_and(|a| a.starts_with('-'))) {
        bail!("{usage}");
    }
    Ok((propose, question))
}

fn run(args: &[String]) -> Result<()> {
    let (propose, question) = parse(args)?;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let cfg = doctor::Config::load(Path::new(SETTINGS), &home)?;
    let machine = Machine { home };
    doctor::ask(&cfg, &machine, &question, propose, &mut std::io::stdout(), &mut std::io::stderr())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_it_is_asked() {
        let a = |s: &[&str]| super::parse(&s.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["why", "slow?"]).unwrap(), (false, "why slow?".into()));
        assert_eq!(a(&["--propose", "--", "install", "htop"]).unwrap(), (true, "install htop".into()));
        assert_eq!(a(&["--", "-x"]).unwrap(), (false, "-x".into()));
        assert!(a(&["-x"]).is_err() && a(&[]).is_err() && a(&["--propose"]).is_err());
    }
}
