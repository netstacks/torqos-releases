//! The filters after a `show` (`show configuration | grep host | wc -l`): the real Linux tools, each
//! given the output before it on its standard input, with your arguments as typed (ADR-017).
//!
//! A pipe filters output; it must not be a way to run programs or touch files (`sort
//! --compress-program=…`, `sort -o ~/.bashrc`, `head ~/.ssh/id_ed25519`): torqcli is what scripts
//! and, later, the agent are given. So each filter runs in a bubblewrap sandbox with no network, no
//! home, no files but the read-only Nix store and an empty /tmp, and nothing from the environment
//! but the locale, from tools fixed at build time (package.nix) — and with a time and size limit.
//! A build without the sandbox runs no filter at all.

use crate::history;
use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A filter: the real tool's name, the help `?` shows, and what it cannot run without (as `?`
/// shows it, and an example), if anything.
pub struct Tool {
    pub name: &'static str,
    pub help: &'static str,
    pub needs: Option<(&'static str, &'static str)>,
}

/// The filters.
pub const FILTERS: &[Tool] = &[
    Tool { name: "cut", help: "Parts of each line (cut -d' ' -f2)", needs: Some(("<what to keep>", "| cut -d' ' -f2")) },
    Tool { name: "grep", help: "Only the lines matching a pattern (grep -v, -i, -E, -c …)", needs: Some(("<pattern>", "| grep host")) },
    Tool { name: "head", help: "The first lines (head -n 5)", needs: None },
    Tool { name: "sort", help: "Sorted (sort -r, -u, -k2 …)", needs: None },
    Tool { name: "tail", help: "The last lines (tail -n 5)", needs: None },
    Tool { name: "tr", help: "Characters replaced or deleted (tr a-z A-Z)", needs: Some(("<characters>", "| tr a-z A-Z")) },
    Tool { name: "uniq", help: "Repeated lines once (uniq -c)", needs: None },
    Tool { name: "wc", help: "Count lines, words, characters (wc -l)", needs: None },
];

/// The filter named `name`.
pub fn tool(name: &str) -> Option<&'static Tool> {
    FILTERS.iter().find(|t| t.name == name)
}

/// One filter: the tool and its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub tool: String,
    pub args: Vec<String>,
}

/// The sandbox (bwrap) and the tools' folder, as the package names them at build time; None in a
/// build without them (a Mac, or cargo outside `nix develop`).
fn sandbox() -> Option<(&'static str, &'static str, &'static str)> {
    Some((option_env!("TORQ_BWRAP")?, option_env!("TORQ_FILTER_BIN")?, option_env!("TORQ_FILTER_SH")?))
}

/// Run before the sandbox, by bash: close every descriptor past stderr that torqcli was given (by an
/// agent's harness, a script, a log pipe), then become the sandbox. (`exec {n}>&-` closes the one
/// numbered `$n`; closing one already closed, the glob's own, is no error.) What torqcli opens itself
/// is close-on-exec already: std opens every descriptor so.
const CLOSE_INHERITED: &str = "for f in /proc/self/fd/*; do n=${f##*/}; case $n in 0|1|2) ;; *) exec {n}>&- ;; esac; done; exec \"$@\"";

/// The most output one filter may give, and how long it may take.
const MAX_OUTPUT: u64 = 64 << 20;
const MAX_TIME: Duration = Duration::from_secs(30);
/// The size of the sandbox's /tmp (where a big `sort` spills), in bytes.
const TMP_SIZE: &str = "67108864";

/// The locale variables passed in, so `sort` and `grep -i` behave as in your terminal.
const LOCALE: &[&str] = &["LANG", "LANGUAGE", "LC_ALL", "LC_COLLATE", "LC_CTYPE", "LC_MESSAGES", "LC_NUMERIC"];

/// The sandboxed command for `f`.
fn command(f: &Filter) -> Result<Command> {
    let (bwrap, bin, sh) = sandbox().context("filters are not available in this build (it has no sandbox for them)")?;
    let mut c = Command::new(sh);
    c.env_clear();
    c.args(["-c", CLOSE_INHERITED, "torq-filter", bwrap]);
    c.args(["--unshare-user", "--unshare-all", "--disable-userns", "--die-with-parent", "--new-session", "--clearenv"]);
    c.args(["--ro-bind", "/nix/store", "/nix/store", "--size", TMP_SIZE, "--tmpfs", "/tmp", "--chdir", "/"]);
    for k in LOCALE {
        if let Ok(v) = std::env::var(k) {
            c.args(["--setenv", k, &v]);
        }
    }
    // NixOS points LOCALE_ARCHIVE through /run; inside, only its store path exists.
    if let Some(a) = std::env::var_os("LOCALE_ARCHIVE").and_then(|a| std::fs::canonicalize(a).ok()).filter(|a| a.starts_with("/nix/store/")) {
        c.arg("--setenv").arg("LOCALE_ARCHIVE").arg(a);
    }
    // Named as people know it (`grep: …`, not its store path), in what it says.
    c.args(["--argv0", &f.tool]);
    c.arg("--").arg(Path::new(bin).join(&f.tool)).args(&f.args);
    Ok(c)
}

/// `out` through the filters, in order.
pub fn apply(filters: &[Filter], out: String) -> Result<String> {
    let mut text = out.into_bytes();
    for f in filters {
        text = run(f, text, MAX_TIME, MAX_OUTPUT)?;
    }
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// Read at most `max` bytes (and one more, to tell), from a thread; the pipe closes when it stops.
fn read_from<R: Read + Send + 'static>(r: Option<R>, max: u64) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(r) = r {
            let _ = r.take(max + 1).read_to_end(&mut buf);
        }
        buf
    })
}

fn run(f: &Filter, input: Vec<u8>, max_time: Duration, max_output: u64) -> Result<Vec<u8>> {
    // Its own process group: a stop must reach bubblewrap's own child too. Killed in the first
    // moments, before that child has armed --die-with-parent, it would live on as an orphan holding the
    // filter's pipes (seen: a test waiting forever); it is still in this group then.
    let mut child = std::os::unix::process::CommandExt::process_group(&mut command(f)?, 0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("| {}: could not start it", f.tool))?;
    // Every pipe from its own thread, so none can fill up and stall the others; a filter that stops
    // reading early (head) only ends the writing, and one that writes too much finds its output closed.
    let mut stdin = child.stdin.take().context("a filter's input")?;
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let (stdout, stderr) = (read_from(child.stdout.take(), max_output), read_from(child.stderr.take(), 64 << 10));
    let deadline = Instant::now() + max_time;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            // The whole process group this filter was started in (its id is the group's): insurance for
            // the race above, which a_stopped_filter_leaves_nothing_running no longer hits reliably (since
            // a bash step starts the sandbox), so no test fails without it.
            let _ = rustix::process::kill_process_group(rustix::process::Pid::from_child(&child), rustix::process::Signal::KILL);
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let (out, err) = (stdout.join().unwrap_or_default(), stderr.join().unwrap_or_default());
    let _ = writer.join();
    let Some(status) = status else { bail!("| {}: stopped after {} seconds", f.tool, max_time.as_secs()) };
    if out.len() as u64 > max_output {
        bail!("| {}: stopped at {} KiB of output", f.tool, max_output >> 10);
    }
    // grep says 1 when no line matched: that is an empty answer, not a failure.
    let nothing = f.tool == "grep" && status.code() == Some(1);
    if !status.success() && !nothing {
        let why = String::from_utf8_lossy(&err);
        // The tool names itself (`head: cannot open …`): said once, as `| head: cannot open …`.
        let first = why.lines().map(str::trim).find(|l| !l.is_empty());
        let why = first.map(|l| history::one_line(l.strip_prefix(&format!("{}: ", f.tool)).unwrap_or(l), 200));
        match why {
            Some(why) => bail!("| {}: {why}", f.tool),
            None => bail!("| {}: it failed ({status})", f.tool),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(line: &str) -> Vec<Filter> {
        let w = crate::session::words(line).unwrap();
        w.split(|x| x == "|").map(|seg| Filter { tool: seg[0].clone(), args: seg[1..].to_vec() }).collect()
    }
    const OUT: &str = "system {\n    host-name two;\n    time-zone UTC;\n}\n";

    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn the_real_tools_with_their_own_options() {
        let a = |line: &str| apply(&f(line), OUT.to_string()).unwrap_or_else(|e| panic!("{line}: {e:#}"));
        assert_eq!(a("grep host"), "    host-name two;\n");
        assert_eq!(a("grep -v -e host -e zone"), "system {\n}\n");
        assert_eq!(a("grep -ic HOST"), "1\n");
        assert_eq!(a("grep -E 'host|zone' | wc -l"), "2\n");
        assert_eq!(a(r"grep '^\s+t'"), "", "basic regular expressions, as grep");
        assert_eq!(a(r"grep -P '^\s+t'"), "    time-zone UTC;\n");
        assert_eq!(a("grep nothing"), "", "no match is an empty answer");
        assert_eq!(a("head -n 2 | tail -n 1"), "    host-name two;\n");
        assert_eq!(a("grep -o '[a-z]*-[a-z]*' | sort -r"), "time-zone\nhost-name\n");
        assert_eq!(a("grep -o '[a-z]*-[a-z]*' | tr a-z A-Z"), "HOST-NAME\nTIME-ZONE\n");
        assert_eq!(a("cut -c1-6 | uniq -c | wc -l"), "4\n");
    }

    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn a_tools_own_error_is_shown() {
        let e = format!("{:#}", apply(&f("grep -Q x"), OUT.into()).unwrap_err());
        assert!(e.starts_with("| grep: ") && e.contains("Q"), "{e}");
        let e = format!("{:#}", apply(&f("grep '\\('"), OUT.into()).unwrap_err());
        assert!(e.starts_with("| grep: ") && !e.contains("/nix/store"), "the tool by its name, not its store path: {e}");
        let e = format!("{:#}", apply(&f("head -n x"), OUT.into()).unwrap_err());
        assert!(e.starts_with("| head: "), "{e}");
        assert!(!e.contains("head: head:"), "the tool named once: {e}");
    }

    /// Only the tools on the list are filters: the grammar refuses any other after a `|`.
    #[test]
    fn only_the_filters_on_the_list() {
        let w = |s: &str| crate::session::words(s).unwrap();
        for t in ["sh -c x", "sed s/a/b/", "awk 1", "tee f", "xargs rm", "less", "/bin/grep x", "./grep x", "env grep x"] {
            let line = w(&format!("show services | {t}"));
            let e = format!("{:#}", crate::grammar::read_line(crate::session::Mode::Operational, &line).err().expect("refused"));
            assert!(e.contains("is not a word here"), "{t}: {e}");
        }
    }


    /// On Linux the filters always run sandboxed, from the tools the build fixed: a build without
    /// them fails here rather than skipping the tests above.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_builds_have_the_sandbox() {
        let (bwrap, bin, sh) = sandbox().expect("TORQ_BWRAP, TORQ_FILTER_BIN and TORQ_FILTER_SH (package.nix; `nix develop`)");
        for p in [bwrap, sh] {
            assert!(p.starts_with("/nix/store/") && Path::new(p).exists(), "{p}");
        }
        for t in FILTERS {
            let p = Path::new(bin).join(t.name);
            assert!(p.starts_with("/nix/store/") && p.exists(), "{}", p.display());
        }
    }

    /// What the reviewer of 4b showed a filter could do outside its sandbox: none of it works.
    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn a_filter_reaches_nothing_outside_its_input() {
        // Not in /tmp: the sandbox has its own, so a file there would be hidden anyway.
        let home = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let (secret, target, marker) = (home.path().join("secret"), home.path().join("written"), home.path().join("ran"));
        std::fs::write(&secret, "the secret\n").unwrap();
        let prog = home.path().join("prog");
        std::fs::write(&prog, format!("#!/bin/sh\ntouch {}\ncat\n", marker.display())).unwrap();
        std::fs::set_permissions(&prog, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let big: String = (0..300_000).map(|i| format!("{i}\n")).collect();
        let s = |p: &Path| p.display().to_string();
        for line in [
            format!("head -n 1 {}", s(&secret)),
            format!("grep -r secret {}", s(home.path())),
            "head -n 1 /etc/passwd".to_string(),
            format!("sort -o {}", s(&target)),
            format!("uniq - {}", s(&target)),
            format!("sort -S 64k -T /tmp --compress-program={} -", s(&prog)),
        ] {
            let got = apply(&f(&line), big.clone());
            assert!(!got.as_deref().unwrap_or("").contains("the secret"), "{line}: read outside");
            assert!(!target.exists(), "{line}: wrote outside");
            assert!(!marker.exists(), "{line}: ran a program");
        }
    }

    /// The second review's escape: an open descriptor torqcli was given, used from inside by a
    /// shell a filter's own option starts (`sort --compress-program=<a store bash>` runs the input).
    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn no_open_descriptor_reaches_a_filter() {
        let sh = std::env::var("PATH").unwrap().split(':').map(|d| Path::new(d).join("sh")).find(|p| p.exists());
        let sh = std::fs::canonicalize(sh.expect("a sh on PATH")).unwrap();
        assert!(sh.starts_with("/nix/store/"), "{}", sh.display());
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        // As torqcli would be started holding one: a shell opens descriptor 7 (inheritable, as a
        // harness's or a script's are), then becomes the filter exactly as torqcli starts it.
        let given = |file: &Path, program: &std::ffi::OsStr, args: Vec<&std::ffi::OsStr>| {
            let mut c = Command::new("sh");
            c.arg("-c").arg("exec 7>>\"$0\"; exec \"$@\"").arg(file).arg(program).args(args);
            c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
            c
        };
        // The way of testing works: without the filter's start, the descriptor does reach a program.
        let control = dir.path().join("control");
        let mut c = given(&control, sh.as_os_str(), vec!["-c".as_ref(), "echo escaped >&7".as_ref()]).spawn().unwrap();
        drop(c.stdin.take());
        c.wait().unwrap();
        assert_eq!(std::fs::read_to_string(&control).unwrap(), "escaped\n", "the test's descriptor is not inheritable");
        // Through the filter's start: closed before the sandbox, so nothing reaches it.
        let outside = dir.path().join("outside");
        let filter = command(&f(&format!("sort -S 64k --compress-program={}", sh.display()))[0]).unwrap();
        let mut c = given(&outside, filter.get_program(), filter.get_args().collect()).spawn().unwrap();
        let payload: String = (0..50_000).map(|_| "echo escaped >&7\n").collect();
        let mut stdin = c.stdin.take().unwrap();
        let _ = std::io::Write::write_all(&mut stdin, payload.as_bytes());
        drop(stdin);
        c.wait().unwrap();
        assert_eq!(std::fs::read_to_string(&outside).unwrap_or_default(), "", "a descriptor reached inside");
    }

    #[test]
    fn the_sandbox_tmp_is_small() {
        if sandbox().is_none() {
            return;
        }
        let c = command(&f("wc -l")[0]).unwrap();
        let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
        let tmp = args.windows(4).position(|w| w == ["--size", TMP_SIZE, "--tmpfs", "/tmp"]);
        assert!(tmp.is_some(), "{args:?}");
        assert!(tmp < args.iter().position(|a| *a == "--"), "a sandbox option, not the tool's");
    }
    /// Stopped at once, again and again, no sandbox is left behind (an orphan would hold the filter's
    /// pipes, and whatever waits on them would wait forever).
    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn a_stopped_filter_leaves_nothing_running() {
        let marker = format!("torq-race-{}-{}", std::process::id(), Instant::now().elapsed().as_nanos());
        let probe = Filter { tool: "grep".into(), args: vec![marker.clone()] };
        // Stopped across its first 10 ms: before bubblewrap starts, and while it is starting its child.
        for i in 0..200 {
            let _ = run(&probe, b"x\n".to_vec(), Duration::from_micros(i * 50), MAX_OUTPUT);
        }
        std::thread::sleep(Duration::from_millis(500));
        let left: Vec<String> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|e| std::fs::read(e.ok()?.path().join("cmdline")).ok())
            .map(|c| String::from_utf8_lossy(&c).replace('\0', " "))
            .filter(|c| c.contains(&marker))
            .collect();
        assert!(left.is_empty(), "left running: {left:?}");
    }

    /// From the audit of 2026-10-31: could a process a filter starts, still holding its output open
    /// after the filter itself exits, keep torqcli waiting? No: the sandbox has its own process
    /// namespace, so when the filter ends everything it started ends with it.
    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn what_a_filter_starts_ends_with_it() {
        let sh = std::env::var("PATH").unwrap().split(':').map(|d| Path::new(d).join("sh")).find(|p| p.exists());
        let sh = std::fs::canonicalize(sh.expect("a sh on PATH")).unwrap();
        // `sort --compress-program` runs its spill through a shell: the input becomes its script.
        let payload: String = (0..50_000).map(|_| "sleep 30 >&2 &\n").collect();
        let start = Instant::now();
        let _ = apply(&f(&format!("sort -S 64k --compress-program={}", sh.display())), payload);
        assert!(start.elapsed() < Duration::from_secs(20), "waited {:?} for what the filter started", start.elapsed());
    }

    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn a_filter_is_stopped_at_its_limits() {
        let big: String = (0..300_000).map(|i| format!("line {i}\n")).collect();
        let e = format!("{:#}", run(&f("grep line")[0], big.clone().into_bytes(), Duration::ZERO, MAX_OUTPUT).unwrap_err());
        assert!(e.contains("stopped after 0 seconds"), "{e}");
        let e = format!("{:#}", run(&f("grep line")[0], big.into_bytes(), MAX_TIME, 4 << 10).unwrap_err());
        assert!(e.contains("stopped at 4 KiB of output"), "{e}");
    }
    #[cfg(target_os = "linux")] // the sandbox
    #[test]
    fn a_large_output_does_not_stall() {
        let big: String = (0..200_000).map(|i| format!("line {i}\n")).collect();
        assert_eq!(apply(&f("wc -l"), big.clone()).unwrap(), "200000\n");
        assert_eq!(apply(&f("grep line | wc -l"), big.clone()).unwrap(), "200000\n", "a large output both ways");
        assert_eq!(apply(&f("sort -S 64k | tail -n 1"), big.clone()).unwrap(), "line 99999\n", "a big sort spills into its own /tmp");
        assert_eq!(apply(&f("head -n 1"), big).unwrap(), "line 0\n", "head stops reading early");
    }
}
