//! Packages: installed software is JSON in the config repo (ADR-011). The pure parts live
//! here; `main.rs` does the I/O (reading the repo, evaluating, talking to the broker).

use anyhow::{Context, Result};

/// The user names the broker accepts as user tier (broker/src/policy.rs, `is_user_data`).
pub fn valid_user(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some('a'..='z' | '_')) && c.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-'))
}

/// A failed pre-check (the Nix evaluation of the changed configuration), as a person needs it:
/// what is wrong and what to do. Nothing has been proposed at this point.
pub fn explain_check_error(msg: &str) -> String {
    let name = msg.split('\'').nth(1).unwrap_or_default();
    if msg.contains("is not a package in nixpkgs") {
        format!("\"{name}\" isn't a package. Find the right name: show packages available {name}")
    } else if let Some((_, how)) = msg.split_once(" is unfree: ") {
        format!("{name} is unfree: {how}")
    } else if msg.contains("is not available here") {
        format!("{name} can't be installed here: it is broken, insecure, or not built for this machine.")
    } else if msg.contains("is not a package name") {
        format!("\"{name}\" isn't a package name. Find one: show packages available <words>")
    } else {
        format!("This change wouldn't work: {msg}")
    }
}

/// An AuthorizedKeysFile pattern as sshd expands it for one user: `%%`, `%h` (home), `%u` (name)
/// and `%U` (uid), in one left-to-right pass; a relative result is relative to the home directory.
/// Any other token is refused rather than guessed at.
pub fn expand_keys_pattern(pattern: &str, home: &str, user: &str, uid: &str) -> Result<String> {
    let mut out = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('h') => out.push_str(home),
            Some('u') => out.push_str(user),
            Some('U') if uid.is_empty() => anyhow::bail!("AuthorizedKeysFile pattern '{pattern}' names the owner's uid, which is not known yet (none declared, and no such user here)"),
            Some('U') => out.push_str(uid),
            other => anyhow::bail!("AuthorizedKeysFile pattern '{pattern}' uses %{} — not understood here", other.map(String::from).unwrap_or_default()),
        }
    }
    Ok(if out.starts_with('/') { out } else { format!("{}/{out}", home.trim_end_matches('/')) })
}

/// What `ssh-keygen -l` says an authorized_keys line is (`256 SHA256:… alice@laptop (ED25519)`), or
/// None if it is not a key: OpenSSH's own reading, not a guess from its shape (audit of 2026-11-01).
pub fn key_id(line: &str) -> Result<Option<String>> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("ssh-keygen")
        .args(["-l", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("running ssh-keygen")?;
    child.stdin.take().context("ssh-keygen's input")?.write_all(line.as_bytes())?;
    let out = child.wait_with_output()?;
    Ok(out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

/// What an authorized_keys line's options mean for who gets in, as a prefix for its description:
/// a `cert-authority` line is not one key but everyone holding a certificate that CA signed.
pub fn key_options_note(line: &str) -> String {
    let line = line.trim();
    let is_key_type = |t: &str| t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-");
    if line.split_whitespace().next().is_some_and(is_key_type) {
        return String::new();
    }
    // Read the options as sshd does: they end at the first space or tab outside double quotes, and
    // are separated by commas outside quotes; names are case-insensitive.
    let mut names = Vec::new();
    let (mut current, mut quoted, mut end) = (String::new(), false, line.len());
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ' ' | '\t' if !quoted => {
                end = i;
                break;
            }
            ',' if !quoted => {
                names.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        if !quoted && c != '"' {
            current.push(c);
        }
    }
    names.push(current);
    let names: Vec<String> = names.iter().map(|o| o.split('=').next().unwrap_or("").trim().to_ascii_lowercase()).collect();
    let opts: String = line[..end].chars().take(60).collect();
    if names.iter().any(|n| n == "cert-authority") {
        "CERTIFICATE AUTHORITY — admits anyone with a certificate it signed: ".into()
    } else if names.iter().any(|n| n == "no-touch-required") {
        format!("no touch needed on the hardware key ({opts}): ")
    } else {
        format!("with options ({opts}): ")
    }
}

/// One input a `flake.lock` change moves: its name, and when it was last changed before and after
/// (seconds; None: not there, or no date). Inputs added and removed are moves too.
pub struct Move {
    pub name: String,
    pub before: Option<u64>,
    pub after: Option<u64>,
    /// In the lock before / after.
    pub was: bool,
    pub is: bool,
}

pub fn lock_moves(old: &str, new: &str) -> Vec<Move> {
    let parse = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap_or_default();
    let (old, new) = (parse(old), parse(new));
    let secs = |v: &serde_json::Value| v["locked"]["lastModified"].as_u64();
    let names = |v: &serde_json::Value| v["nodes"].as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default();
    let mut all: Vec<String> = names(&new).into_iter().chain(names(&old)).filter(|n| n != "root").collect();
    all.sort();
    all.dedup();
    all.into_iter()
        .filter_map(|name| {
            let (a, b) = (&old["nodes"][&name], &new["nodes"][&name]);
            (a["locked"] != b["locked"]).then(|| Move { before: secs(a), after: secs(b), was: !a.is_null(), is: !b.is_null(), name })
        })
        .collect()
}

/// A move's dates as shown: the day, and the time too when both fall on the same day (a path input
/// during development); `?` without a date.
pub fn move_dates(m: &Move) -> (String, String) {
    let same_day = matches!((m.before, m.after), (Some(a), Some(b)) if ymd(a) == ymd(b));
    let when = |v: Option<u64>| match v {
        Some(s) if same_day => format!("{} {:02}:{:02}", ymd(s), s % 86_400 / 3_600, s % 3_600 / 60),
        Some(s) => ymd(s),
        None => "?".into(),
    };
    (when(m.before), when(m.after))
}

/// A Unix time as a UTC date, "2026-10-30" (civil-from-days, no time-zone database needed).
pub fn ymd(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names() {
        for ok in ["viper", "_svc", "a-b_9"] {
            assert!(valid_user(ok), "{ok}");
        }
        for bad in ["", "Viper", "-x", "a.b", "a/b", "..", "ca sey"] {
            assert!(!valid_user(bad), "{bad}");
        }
    }

    #[test]
    fn dates_and_lock_changes() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_793_386_952), "2026-10-30");
        assert_eq!(ymd(951_782_400), "2000-02-29");
        let lock = |kt: u64, nt: u64| {
            format!(r#"{{"nodes":{{"torq":{{"locked":{{"lastModified":{kt},"narHash":"a{kt}"}}}},"nixpkgs":{{"locked":{{"lastModified":{nt}}}}},"root":{{}}}}}}"#)
        };
        let moved = lock_moves(&lock(1_793_000_000, 1_790_000_000), &lock(1_793_386_952, 1_790_000_000));
        assert_eq!(moved.iter().map(|m| (m.name.as_str(), move_dates(m))).collect::<Vec<_>>(), [("torq", ("2026-10-26".to_string(), "2026-10-30".to_string()))]);
        let same = lock_moves(&lock(1_793_380_000, 1), &lock(1_793_386_952, 1));
        assert_eq!(move_dates(&same[0]), ("2026-10-30 17:06".to_string(), "2026-10-30 19:02".to_string())); // date -u -d @1793380000
        let gone = lock_moves(&lock(1, 1), r#"{"nodes":{"root":{},"torq":{"locked":{"lastModified":1,"narHash":"a1"}}}}"#);
        assert!(gone.iter().any(|m| m.name == "nixpkgs" && m.was && !m.is), "an input removed is a move too");
    }

    #[test]
    fn errors_say_what_to_do() {
        assert_eq!(
            explain_check_error("torq.packages: 'htpo' (user viper) is not a package in nixpkgs"),
            "\"htpo\" isn't a package. Find the right name: show packages available htpo"
        );
        assert_eq!(
            explain_check_error("torq.packages: 'vscode' (user viper) is unfree: allow it with `set torq unfree packages vscode`, or every unfree package with `set torq unfree all true`"),
            "vscode is unfree: allow it with `set torq unfree packages vscode`, or every unfree package with `set torq unfree all true`"
        );
        assert!(explain_check_error("torq.packages: 'vscode' (user viper) is not available here: broken, ...")
            .starts_with("vscode can't be installed here"));
        assert!(explain_check_error("something else").starts_with("This change wouldn't work"));
    }

    #[test]
    fn keys_patterns_expand_like_sshd() {
        let e = |p| expand_keys_pattern(p, "/home/viper", "viper", "1000");
        assert_eq!(e("%h/.ssh/authorized_keys").unwrap(), "/home/viper/.ssh/authorized_keys");
        assert_eq!(e("/etc/ssh/authorized_keys.d/%u").unwrap(), "/etc/ssh/authorized_keys.d/viper");
        assert_eq!(e("/keys/%U").unwrap(), "/keys/1000");
        assert_eq!(e("/odd/%%h").unwrap(), "/odd/%h", "one pass: %% is a literal %");
        assert_eq!(e(".ssh/authorized_keys2").unwrap(), "/home/viper/.ssh/authorized_keys2", "relative to home");
        assert!(e("/keys/%i").is_err() && e("/keys/%").is_err());
        // A new owner with no uid yet: a pattern naming it cannot say whose file, so it is refused.
        assert!(expand_keys_pattern("/keys/%U", "/home/new", "new", "").is_err());
        assert!(expand_keys_pattern("%h/.ssh/authorized_keys", "/home/new", "new", "").is_ok());
    }

    #[test]
    fn key_option_notes() {
        assert_eq!(key_options_note("ssh-ed25519 AAAA me@mac"), "");
        assert!(key_options_note("cert-authority ssh-ed25519 AAAA ca").starts_with("CERTIFICATE AUTHORITY"));
        assert!(key_options_note("cert-authority,principals=\"a b\" ecdsa-sha2-nistp256 AAAA").starts_with("CERTIFICATE"));
        let r = key_options_note("command=\"echo hi there\",from=\"10.0.0.1\" ssh-rsa AAAA x");
        assert!(r.starts_with("with options (command=\"echo hi there\",from=\"10.0.0.1\")"), "{r}");
        // The reviewer's ways to make a CA look like a restricted key — sshd accepts all three.
        for ca in [
            "cert-authority\tssh-ed25519 AAAA ca",
            "command=\"x ssh-\",cert-authority ssh-ed25519 AAAA ca",
            "CERT-AUTHORITY ssh-ed25519 AAAA ca",
        ] {
            assert!(key_options_note(ca).starts_with("CERTIFICATE AUTHORITY"), "{ca:?}");
        }
        // ...and a CA mentioned only inside a quoted command is not one.
        assert!(key_options_note("command=\"a,cert-authority,b\" ssh-ed25519 AAAA").starts_with("with options"));
        assert!(key_options_note("no-touch-required sk-ssh-ed25519@openssh.com AAAA").starts_with("no touch"));
    }

}
