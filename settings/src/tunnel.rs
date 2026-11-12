//! `torq-tunnel`: what `torq-tunnel@<name>.service` runs (ADR-050 2). It reads the person's tunnel of that
//! name and becomes ssh with the one command line the library makes: nothing of the file reaches ssh but a
//! checked destination and checked forwards. `torq-tunnel check` says whether the whole file is right, and
//! `torq-tunnel status` what each tunnel is doing, in the panel's words, for a person at a terminal.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};
use torq::tunnels::{self, State};

/// This tunnel itself is wrong, or gone from the file (sysexits' EX_CONFIG): the unit does not start it again
/// until the person does (`RestartPreventExitStatus=78`), since it would be refused every time. Another
/// tunnel's mistake, or a file being edited that is not TOML for a moment, is not this: that is tried again.
const CONFIG: u8 = 78;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["check"] => match tunnels::load_strict() {
            Ok(all) => {
                for t in &all {
                    println!("{}  {}", t.name, t.describe());
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::from(CONFIG)
            }
        },
        ["status"] => match tunnels::load() {
            Ok(file) => {
                for (t, (state, login)) in file.tunnels.iter().zip(tunnels::status(&file.tunnels)) {
                    let (word, why) = match state {
                        State::Off => ("off", String::new()),
                        State::Connecting => ("connecting", String::new()),
                        State::Up => ("up", String::new()),
                        State::Retrying(w) => ("retrying", w),
                        State::Failed(w) => ("stopped", w),
                    };
                    println!("{}\t{word}\t{}\t{why}", t.name, if login { "at-login" } else { "-" });
                }
                for (n, why) in &file.wrong {
                    println!("{n}\twrong\t-\t{why}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
        },
        ["run", name] => {
            let file = match tunnels::load() {
                Ok(f) => f,
                // Not TOML (half saved?) or unreadable: tried again, so fixing the file brings it back.
                Err(e) => {
                    eprintln!("{e:#}");
                    return ExitCode::FAILURE;
                }
            };
            if let Some((_, why)) = file.wrong.iter().find(|(n, _)| n == name) {
                eprintln!("This tunnel is not right: {why}");
                return ExitCode::from(CONFIG);
            }
            let Some(t) = file.tunnels.iter().find(|t| t.name == *name) else {
                eprintln!("There is no tunnel `{name}` in {}.", tunnels::file().map(|p| p.display().to_string()).unwrap_or_default());
                return ExitCode::from(CONFIG);
            };
            eprintln!("Connecting to {}", t.describe());
            // Becomes ssh: systemd watches ssh itself, and its words are the journal's.
            let e = Command::new("ssh").args(t.ssh_args()).exec();
            eprintln!("cannot run ssh: {e}");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("usage: torq-tunnel run <name> | torq-tunnel check | torq-tunnel status");
            ExitCode::from(2)
        }
    }
}
