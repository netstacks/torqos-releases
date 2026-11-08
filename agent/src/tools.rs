//! Doctor mode's tools (ADR-042): read-only, all of them, and nothing else. The set is closed — an
//! enum, matched by exact name — so a tool the model names that is not here (`propose`, `commit`, a
//! shell) does not exist to be called. Each runs a fixed program by its absolute path, with its
//! arguments as separate words (no shell), from parameters checked against narrow patterns: what a
//! log said, the model repeating it, cannot become a command, an option or a second torqcli command.

use crate::clean::clean;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const JOURNALCTL: &str = "/run/current-system/sw/bin/journalctl";
pub const TORQCLI: &str = "/run/current-system/sw/bin/torqcli";
/// What a tool returns to the model, at most (the last of it, for a log).
pub const MAX_OUTPUT: usize = 16_000;

/// Every tool the agent has. Doctor mode's (`ALL`) only read; `Propose` (propose mode alone,
/// ADR-042, 6) proposes a change through torqcli as the agent — which stops at the proposal: a
/// person applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Journal,
    Hardware,
    Configuration,
    Ledger,
    Alarms,
    Options,
    Propose,
}

/// What a tool does once its parameters are checked: programs to run, or the options to look up.
#[derive(Debug, PartialEq)]
pub enum Action {
    Run(Vec<(PathBuf, Vec<String>)>),
    Options(Vec<String>),
    /// torqcli's script (`-c`) that proposes a change, as the agent, naming its session.
    Propose(String),
}

/// Runs a program and gives its output (standard output and error): the machine, or a test's stand-in.
pub trait Host {
    fn run(&self, program: &Path, args: &[String]) -> Result<String>;
    /// torqcli with `script` (`-c`), as the agent, its session at the broker `session` (the only
    /// way a proposal names one: `TORQ_AGENT_SESSION`).
    fn propose(&self, script: &str, session: &str) -> Result<String>;
    /// torqcli's `agent session <args>` (start, taint): the agent's session at the broker. An error
    /// unless it succeeded — a report that did not reach the broker is never taken as made.
    fn session(&self, args: &[&str]) -> Result<String>;
}

impl Tool {
    pub const ALL: [Tool; 6] = [Tool::Journal, Tool::Hardware, Tool::Configuration, Tool::Ledger, Tool::Alarms, Tool::Options];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Journal => "read_journal",
            Tool::Hardware => "hardware_facts",
            Tool::Configuration => "show_configuration",
            Tool::Ledger => "show_ledger",
            Tool::Alarms => "show_alarms",
            Tool::Options => "nixos_option_docs",
            Tool::Propose => "propose_change",
        }
    }

    /// The tool of that exact name, among those offered.
    pub fn named(name: &str, offered: &[Tool]) -> Option<Tool> {
        offered.iter().copied().find(|t| t.name() == name)
    }

    /// Whether what it returns is untrusted text — written by anyone who can log, a USB device's
    /// descriptor, a proposal's reason, a configuration value (a network's name, a description,
    /// a file someone attached) — so a session that read it is tainted (AI-LAYER.md). Only NixOS's
    /// own option docs are not.
    pub fn taints(self) -> bool {
        !matches!(self, Tool::Options)
    }

    fn describe(self) -> (&'static str, Value) {
        let none = json!({ "type": "object", "properties": {}, "additionalProperties": false });
        match self {
            Tool::Journal => ("Read the system journal (journalctl), newest last. Untrusted text.", json!({
                "type": "object",
                "properties": {
                    "unit": { "type": "string", "description": "a systemd unit, e.g. NetworkManager.service, wpa_supplicant.service" },
                    "kernel": { "type": "boolean", "description": "kernel messages only (dmesg)" },
                    "boot": { "type": "string", "enum": ["current", "previous"] },
                    "priority": { "type": "string", "enum": ["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"], "description": "this and more severe" },
                    "grep": { "type": "string", "description": "only lines matching this pattern" },
                    "lines": { "type": "integer", "minimum": 1, "maximum": 400 }
                },
                "additionalProperties": false
            })),
            Tool::Hardware => ("Facts about the hardware: its devices, drivers and network interfaces.", json!({
                "type": "object",
                "properties": { "what": { "type": "string", "enum": ["pci", "usb", "network", "wireless", "block", "kernel", "cpu_memory"] } },
                "required": ["what"],
                "additionalProperties": false
            })),
            Tool::Configuration => ("The machine's configuration as torqcli shows it (`show configuration [<path>]`). Untrusted text (values).", json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "words of the tree, e.g. \"networking wireless\" or \"services openssh\"; none: all of it" } },
                "additionalProperties": false
            })),
            Tool::Ledger => ("The ledger of changes to this machine (`show system ledger`). Untrusted text (reasons).", none),
            Tool::Alarms => ("What is wrong with the machine now (`show system alarms`). Untrusted text.", none),
            Tool::Options => ("NixOS's documentation of the settings under a path: each one's type and one line of help.", json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "e.g. \"networking.wireless\" or \"networking networkmanager wifi\"" } },
                "required": ["path"],
                "additionalProperties": false
            })),
            Tool::Propose => ("Propose one change to the configuration, for a person to apply: settings to set and to delete, as words of the tree, and why. It is never applied by this: a person reads it and applies it (`request system apply <n>`); the broker refuses what the agent may not propose. Only what you were asked for.", json!({
                "type": "object",
                "properties": {
                    "reason": { "type": "string", "description": "one line, at most 200 characters: letters, digits, spaces and , . : ( ) _ / + -" },
                    "set": { "type": "array", "maxItems": 8, "items": { "type": "object", "properties": {
                        "path": { "type": "string", "description": "words of the tree, e.g. \"users viper packages\" or \"networking wireless enable\"" },
                        "value": { "type": ["string", "integer", "boolean"], "description": "one value: a word (a package's name), a number, true or false" }
                    }, "required": ["path", "value"], "additionalProperties": false } },
                    "delete": { "type": "array", "maxItems": 8, "items": { "type": "string", "description": "words of the tree, e.g. \"users viper packages htop\"" } }
                },
                "required": ["reason"],
                "additionalProperties": false
            })),
        }
    }

    /// The tools as the chat API is told them.
    pub fn definitions(offered: &[Tool]) -> Value {
        Value::Array(offered.iter().map(|t| {
            let (description, parameters) = t.describe();
            json!({ "type": "function", "function": { "name": t.name(), "description": description, "parameters": parameters } })
        }).collect())
    }

    /// The tool's parameters checked, as what it will do. Anything not in its schema is refused.
    pub fn plan(self, args: &Value) -> Result<Action> {
        let empty = Map::new();
        let args = match args {
            Value::Object(m) => m,
            Value::Null => &empty,
            _ => bail!("the arguments are not an object"),
        };
        let (_, schema) = self.describe();
        let known = schema["properties"].as_object().map(|p| p.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
        if let Some(k) = args.keys().find(|k| !known.contains(k)) {
            bail!("{} has no parameter {k:?}", self.name());
        }
        let text = |k: &str| -> Result<Option<&str>> {
            match args.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) => Ok(Some(s)),
                Some(_) => bail!("{k}: a string"),
            }
        };
        let torqcli = |command: String| Action::Run(vec![(PathBuf::from(TORQCLI), vec!["-c".into(), format!("{command} | no-more")])]);
        Ok(match self {
            Tool::Journal => {
                let mut a: Vec<String> = ["--no-pager", "--quiet", "--output=short-iso"].map(String::from).to_vec();
                if let Some(u) = text("unit")? {
                    if !word(u, "@._:-", 128) {
                        bail!("unit {u:?}: not a unit name");
                    }
                    a.push(format!("--unit={u}"));
                }
                match args.get("kernel") {
                    None | Some(Value::Null) | Some(Value::Bool(false)) => {}
                    Some(Value::Bool(true)) => a.push("--dmesg".into()),
                    Some(_) => bail!("kernel: true or false"),
                }
                match text("boot")? {
                    None => {}
                    Some("current") => a.push("--boot=0".into()),
                    Some("previous") => a.push("--boot=-1".into()),
                    Some(b) => bail!("boot {b:?}: current or previous"),
                }
                if let Some(p) = text("priority")? {
                    if !["emerg", "alert", "crit", "err", "warning", "notice", "info", "debug"].contains(&p) {
                        bail!("priority {p:?}: not a priority");
                    }
                    a.push(format!("--priority={p}"));
                }
                if let Some(g) = text("grep")? {
                    if g.is_empty() || g.chars().count() > 100 || g.chars().any(char::is_control) {
                        bail!("grep: a pattern of 1 to 100 characters, on one line");
                    }
                    a.push(format!("--grep={g}"));
                }
                let lines = match args.get("lines") {
                    None | Some(Value::Null) => 200,
                    Some(v) => v.as_u64().filter(|n| (1..=400).contains(n)).context("lines: 1 to 400")?,
                };
                a.push(format!("--lines={lines}"));
                Action::Run(vec![(PathBuf::from(JOURNALCTL), a)])
            }
            Tool::Hardware => {
                let bin = |p: &str| PathBuf::from(format!("{}/{p}", option_env!("TORQ_DOCTOR_BIN").unwrap_or("/run/current-system/sw/bin")));
                let r = |p: &str, a: &[&str]| (bin(p), a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
                Action::Run(match text("what")? {
                    Some("pci") => vec![r("lspci", &["-nnk"])],
                    Some("usb") => vec![r("lsusb", &[])],
                    Some("network") => vec![r("ip", &["-details", "-statistics", "link"]), r("ip", &["address"]), r("ip", &["route"])],
                    Some("wireless") => vec![r("iw", &["dev"]), r("rfkill", &["list"])],
                    Some("block") => vec![r("lsblk", &["-o", "NAME,SIZE,TYPE,FSTYPE,MOUNTPOINTS,MODEL"])],
                    Some("kernel") => vec![r("uname", &["-a"]), r("lsmod", &[])],
                    Some("cpu_memory") => vec![r("lscpu", &[]), r("free", &["-h"])],
                    Some(w) => bail!("what {w:?}: not one of the hardware facts"),
                    None => bail!("what: which facts"),
                })
            }
            Tool::Configuration => match text("path")? {
                None => torqcli("show configuration".into()),
                Some(p) => torqcli(format!("show configuration {}", words(p)?.join(" "))),
            },
            Tool::Ledger => torqcli("show system ledger".into()),
            Tool::Alarms => torqcli("show system alarms".into()),
            Tool::Options => Action::Options(words(text("path")?.context("path: which settings")?)?),
            Tool::Propose => Action::Propose(proposal(args)?),
        })
    }
}

/// `s` is one word: a letter or digit first, then those or the characters of `also`, at most `max`.
fn word(s: &str, also: &str, max: usize) -> bool {
    let mut c = s.chars();
    s.len() <= max && c.next().is_some_and(|f| f.is_ascii_alphanumeric()) && c.all(|x| x.is_ascii_alphanumeric() || x == '_' || also.contains(x))
}

/// A path of the tree, as words (`networking wireless`, or `networking.wireless`): 1 to 8, each a
/// plain name — so nothing that torqcli would read as a second command, a filter or a quote.
fn words(path: &str) -> Result<Vec<String>> {
    let w: Vec<String> = path.split([' ', '.']).filter(|s| !s.is_empty()).map(String::from).collect();
    if w.is_empty() || w.len() > 8 || !w.iter().all(|x| word(x, "-", 64)) {
        bail!("path {path:?}: up to 8 names of the tree (letters, digits, - and _), e.g. \"services openssh\"");
    }
    Ok(w)
}

/// What NixOS documents under `path`, from the system's own schema (/etc/torq/nixos-schema.json).
pub fn options(schema: &Path, path: &[String]) -> Result<String> {
    let text = std::fs::read_to_string(schema).with_context(|| format!("reading {}", schema.display()))?;
    let s: Value = serde_json::from_str(&text).with_context(|| format!("reading {}", schema.display()))?;
    let mut out = String::new();
    let mut n = 0;
    for e in s["settings"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let loc: Vec<&str> = e["loc"].as_array().map(|l| l.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if loc.len() < path.len() || loc.iter().zip(path).any(|(a, b)| a != b) {
            continue;
        }
        n += 1;
        if n <= 60 {
            out.push_str(&format!("{} ({}): {}\n", loc.join("."), e["type"]["name"].as_str().unwrap_or("?"), e["help"].as_str().unwrap_or("")));
        }
    }
    if n == 0 {
        return Ok(format!("no NixOS setting under {}", path.join(".")));
    }
    if n > 60 {
        out.push_str(&format!("… and {} more: ask for a narrower path\n", n - 60));
    }
    Ok(out)
}

/// At most `max` characters: the last of them, which for a log is the newest.
pub fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    format!("[… {} characters left out]\n{}", n - max, s.chars().skip(n - max).collect::<String>())
}

/// The most changes one proposal makes.
pub const MAX_CHANGES: usize = 8;

/// `propose_change`'s arguments, checked, as torqcli's script: `configure`, a `set` or `delete` of
/// checked words each, and `commit comment "<reason>"` — which, as the agent, proposes and stops.
/// Nothing in it is a quote, a `;`, a filter or an option of the model's making.
fn proposal(args: &Map<String, Value>) -> Result<String> {
    let reason = args.get("reason").and_then(Value::as_str).context("reason: why, in one line")?;
    let n = reason.chars().count();
    if !(1..=200).contains(&n) || !reason.chars().all(|c| c.is_ascii_alphanumeric() || " ,.:()_/+-".contains(c)) || reason.trim().is_empty() {
        bail!("reason: 1 to 200 characters, letters, digits, spaces and , . : ( ) _ / + - only");
    }
    let list = |k: &str| -> Result<Vec<Value>> {
        match args.get(k) {
            None | Some(Value::Null) => Ok(vec![]),
            Some(Value::Array(a)) => Ok(a.clone()),
            Some(_) => bail!("{k}: a list"),
        }
    };
    let (sets, deletes) = (list("set")?, list("delete")?);
    if sets.len() + deletes.len() == 0 || sets.len() + deletes.len() > MAX_CHANGES {
        bail!("a proposal sets or deletes 1 to {MAX_CHANGES} settings");
    }
    let mut script = vec!["configure".to_string()];
    for s in &sets {
        let o = s.as_object().context("set: each one {path, value}")?;
        if let Some(k) = o.keys().find(|k| *k != "path" && *k != "value") {
            bail!("set has no field {k:?}");
        }
        let path = words(o.get("path").and_then(Value::as_str).context("set: a path")?)?;
        let value = match o.get("value") {
            Some(Value::String(v)) if word(v, "._:/@+-", 128) => v.clone(),
            Some(Value::Bool(b)) => b.to_string(),
            Some(Value::Number(n)) if n.is_i64() => n.to_string(),
            _ => bail!("set {}: a value is one word (letters, digits, . _ : / @ + -), a whole number, or true or false", path.join(" ")),
        };
        script.push(format!("set {} {value}", path.join(" ")));
    }
    for d in &deletes {
        script.push(format!("delete {}", words(d.as_str().context("delete: each one words of the tree")?)?.join(" ")));
    }
    script.push(format!("commit comment \"{reason}\""));
    Ok(script.join("; "))
}

/// Runs the action, and gives what it returned cleaned of escapes and control characters, cut to
/// size. `session`: the agent's session at the broker, which a proposal names.
pub fn perform(action: &Action, host: &dyn Host, schema: &Path, session: &str) -> Result<String> {
    let raw = match action {
        Action::Run(steps) => {
            let mut out = String::new();
            for (program, args) in steps {
                out.push_str(&host.run(program, args)?);
            }
            out
        }
        Action::Options(path) => options(schema, path)?,
        Action::Propose(script) => host.propose(script, session)?,
    };
    Ok(truncate(&clean(&raw), MAX_OUTPUT))
}

/// What a tool's program may print, at most, before it is stopped (both of its outputs together).
pub const MAX_PROGRAM_BYTES: usize = 1 << 20;
/// How long a tool's program may run.
pub const PROGRAM_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a proposal may take: torqcli evaluates the configuration before it proposes, and under a
/// person's trust the broker applies it then and there — a build and a switch.
pub const PROPOSE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Run `cmd`, its input closed, in a process group of its own; its standard output and error
/// together, read only up to `cap` bytes — past that, or past `timeout`, the whole group is killed
/// and the output so far given, with a line saying why.
pub fn bounded(mut cmd: Command, cap: usize, timeout: Duration) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let mut child = cmd.process_group(0).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let group = rustix::process::Pid::from_child(&child);
    let buf = Arc::new(Mutex::new(Vec::new()));
    let full = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel();
    let pipes: Vec<Box<dyn Read + Send>> = vec![Box::new(child.stdout.take().context("its output")?), Box::new(child.stderr.take().context("its errors")?)];
    for mut pipe in pipes {
        let (buf, full, done) = (buf.clone(), full.clone(), done.clone());
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut b = buf.lock().unwrap_or_else(|e| e.into_inner());
                let room = cap.saturating_sub(b.len());
                b.extend_from_slice(&chunk[..n.min(room)]);
                if n > room {
                    full.store(true, Ordering::Relaxed);
                    break;
                }
            }
            let _ = done.send(());
        });
    }
    drop(done);
    let deadline = Instant::now() + timeout;
    let mut why = None;
    let mut closed = 0;
    while closed < 2 {
        if full.load(Ordering::Relaxed) {
            why = Some(format!("more than {cap} bytes"));
            break;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            why = Some(format!("still running after {} s", timeout.as_secs()));
            break;
        }
        match finished.recv_timeout(left.min(Duration::from_millis(50))) {
            Ok(()) => closed += 1,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if why.is_some() {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        let _ = child.kill();
    }
    let status = child.wait()?;
    let mut text = String::from_utf8_lossy(&buf.lock().unwrap_or_else(|e| e.into_inner())).into_owned();
    match why {
        Some(w) => text.push_str(&format!("\n[stopped: {w}]\n")),
        None if !status.success() => text.push_str(&format!("\n[exited with {status}]\n")),
        None => {}
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doctor's limits as numbers: changing one is a decision (the audit of 0.3.0).
    #[test]
    fn the_limits_are_these_numbers() {
        assert_eq!((PROGRAM_TIMEOUT.as_secs(), PROPOSE_TIMEOUT.as_secs()), (30, 30 * 60));
    }

    #[test]
    fn a_program_is_stopped_at_its_bounds() {
        let sh = |script: &str| {
            let mut c = Command::new("/bin/sh");
            c.args(["-c", script]);
            c
        };
        assert_eq!(bounded(sh("echo out; echo err >&2"), 1000, Duration::from_secs(5)).unwrap().lines().count(), 2);
        let t = Instant::now();
        let out = bounded(sh("while :; do echo yyyyyyyyyyyyyyyy; done"), 4096, Duration::from_secs(20)).unwrap();
        assert!(out.len() < 4096 + 100 && out.ends_with("[stopped: more than 4096 bytes]\n"), "{}", out.len());
        // A child of it that keeps the output open is stopped with it (the whole group).
        let out = bounded(sh("(while :; do :; done) & while :; do :; done"), 4096, Duration::from_secs(1)).unwrap();
        assert!(out.ends_with("[stopped: still running after 1 s]\n"), "{out}");
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(bounded(sh("exit 3"), 100, Duration::from_secs(5)).unwrap().contains("[exited with"));
    }

    #[test]
    fn only_read_only_tools_exist() {
        let names: Vec<&str> = Tool::ALL.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["read_journal", "hardware_facts", "show_configuration", "show_ledger", "show_alarms", "nixos_option_docs"]);
        // Every one runs journalctl, torqcli's `show`, or a hardware tool that only reports — or reads a file.
        for t in Tool::ALL {
            let args = match t {
                Tool::Hardware => json!({"what": "network"}),
                Tool::Options => json!({"path": "networking"}),
                _ => json!({}),
            };
            match t.plan(&args).unwrap() {
                Action::Options(_) => {}
                Action::Propose(_) => panic!("doctor mode has no tool that proposes"),
                Action::Run(steps) => for (p, a) in steps {
                    let p = p.file_name().unwrap().to_str().unwrap().to_string();
                    match p.as_str() {
                        "journalctl" => assert!(a.iter().all(|x| !x.contains("vacuum") && !x.contains("rotate") && !x.contains("flush"))),
                        "torqcli" => {
                            assert_eq!(a[0], "-c");
                            assert!(a[1].starts_with("show ") && a[1].ends_with(" | no-more") && !a[1].contains(';'), "{a:?}");
                        }
                        "lspci" | "lsusb" | "ip" | "iw" | "rfkill" | "lsblk" | "uname" | "lsmod" | "lscpu" | "free" => {
                            // `ip` and `iw` and `rfkill` only list here.
                            assert!(a.iter().all(|x| !["set", "add", "del", "block", "unblock", "connect", "disconnect"].contains(&x.as_str())), "{p} {a:?}");
                        }
                        other => panic!("{other}: a program no tool should run"),
                    }
                },
            }
        }
        // Every hardware fact, too.
        for what in ["pci", "usb", "network", "wireless", "block", "kernel", "cpu_memory"] {
            assert!(matches!(Tool::Hardware.plan(&json!({"what": what})).unwrap(), Action::Run(_)));
        }
    }

    #[test]
    fn a_tool_is_named_exactly_and_only_if_offered() {
        assert_eq!(Tool::named("read_journal", &Tool::ALL), Some(Tool::Journal));
        assert_eq!(Tool::named("read_journal", &[Tool::Ledger]), None, "not offered: not there");
        for name in ["propose_change", "commit", "config.propose", "run_shell", "READ_JOURNAL", "read_journal ", ""] {
            assert_eq!(Tool::named(name, &Tool::ALL), None, "{name}");
        }
    }

    #[test]
    fn nothing_from_the_model_becomes_a_command_or_an_option() {
        let refused = [
            (Tool::Configuration, json!({"path": "services; configure; set services openssh enable true; commit"})),
            (Tool::Configuration, json!({"path": "system | display set"})),
            (Tool::Configuration, json!({"path": "services \"x\""})),
            (Tool::Configuration, json!({"path": "-v"})),
            (Tool::Configuration, json!({"path": "a b c d e f g h i"})),
            (Tool::Journal, json!({"unit": "--vacuum-size=1"})),
            (Tool::Journal, json!({"unit": "x y"})),
            (Tool::Journal, json!({"lines": 100000})),
            (Tool::Journal, json!({"lines": "10"})),
            (Tool::Journal, json!({"boot": "-5"})),
            (Tool::Journal, json!({"grep": "a\nb"})),
            (Tool::Journal, json!({"flush": true})),
            (Tool::Hardware, json!({"what": "network; rm -rf /"})),
            (Tool::Hardware, json!({})),
            (Tool::Ledger, json!({"command": "commit"})),
            (Tool::Options, json!({"path": "x;y"})),
            (Tool::Journal, json!("not an object")),
        ];
        for (t, a) in refused {
            assert!(t.plan(&a).is_err(), "{t:?} {a}");
        }
        // What passes is separate words, each one argument.
        assert_eq!(
            Tool::Journal.plan(&json!({"unit": "wpa_supplicant.service", "grep": "--reason; x", "priority": "warning", "boot": "previous"})).unwrap(),
            Action::Run(vec![(PathBuf::from(JOURNALCTL), ["--no-pager", "--quiet", "--output=short-iso", "--unit=wpa_supplicant.service", "--boot=-1", "--priority=warning", "--grep=--reason; x", "--lines=200"].map(String::from).to_vec())])
        );
        assert_eq!(
            Tool::Configuration.plan(&json!({"path": "networking.wireless"})).unwrap(),
            Action::Run(vec![(PathBuf::from(TORQCLI), vec!["-c".into(), "show configuration networking wireless | no-more".into()])])
        );
    }

    /// ADR-042, 6: a proposal is structured — words of the tree and one-word values — and becomes a
    /// torqcli script of the agent's own making, never text of the model's.
    #[test]
    fn a_proposal_is_words_never_a_command() {
        assert_eq!(
            Tool::Propose.plan(&json!({"reason": "htop for viper", "set": [{"path": "users viper packages", "value": "htop"}, {"path": "networking.wireless.enable", "value": true}], "delete": ["users viper packages nano"]})).unwrap(),
            Action::Propose("configure; set users viper packages htop; set networking wireless enable true; delete users viper packages nano; commit comment \"htop for viper\"".into())
        );
        let many: Vec<Value> = (0..9).map(|_| json!({"path": "a", "value": "b"})).collect();
        for bad in [
            json!({"set": [{"path": "a", "value": "b"}]}),
            json!({"reason": "x\"; commit; \"", "set": [{"path": "a", "value": "b"}]}),
            json!({"reason": "x; y", "set": [{"path": "a", "value": "b"}]}),
            json!({"reason": "x", "set": [{"path": "a; commit", "value": "b"}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": "b; delete system"}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": "\"q\""}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": "-x"}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": "b | display set"}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": 1.5}]}),
            json!({"reason": "x", "set": [{"path": "a", "value": "b", "apply": true}]}),
            json!({"reason": "x", "delete": ["a | b"]}),
            json!({"reason": "x"}),
            json!({"reason": "x", "set": many}),
            json!({"reason": "x", "set": [{"path": "a", "value": "b"}], "apply": true}),
        ] {
            assert!(Tool::Propose.plan(&bad).is_err(), "{bad}");
        }
        // Not among doctor mode's tools: offered only in propose mode.
        assert!(!Tool::ALL.contains(&Tool::Propose));
        assert!(Tool::Propose.taints(), "what torqcli says back is not the agent's own text");
    }

    #[test]
    fn option_docs_from_the_schema() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("schema.json");
        std::fs::write(&f, r#"{"settings":[{"loc":["networking","wireless","enable"],"type":{"name":"bool"},"help":"Whether to enable wpa_supplicant."},{"loc":["networking","hostName"],"type":{"name":"str"},"help":"The name."}]}"#).unwrap();
        let p = vec!["networking".to_string(), "wireless".to_string()];
        assert_eq!(options(&f, &p).unwrap(), "networking.wireless.enable (bool): Whether to enable wpa_supplicant.\n");
        assert!(options(&f, &["nope".into()]).unwrap().starts_with("no NixOS setting"));
    }

    #[test]
    fn a_long_output_keeps_its_end() {
        assert_eq!(truncate("abcdef", 3), "[… 3 characters left out]\ndef");
        assert_eq!(truncate("abc", 3), "abc");
    }
}
