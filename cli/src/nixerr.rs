//! What a failed Nix evaluation says, as one line a person can act on. Nix ends its output with the
//! error that stopped it: `error: <message>` — or a bare `error:` with the message on the lines
//! after it (failed assertions, a value of the wrong type) — above which come `… while evaluating`
//! traces. The message is that last error's text, without the traces, source excerpts or hints.

/// The message of the last error in `stderr` (Nix's), its lines joined; None if there is none.
pub fn message(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let start = lines.iter().rposition(|l| l.trim_start().starts_with("error:"))?;
    let mut parts: Vec<String> = Vec::new();
    let first = lines[start].trim_start().trim_start_matches("error:").trim();
    if !first.is_empty() {
        parts.push(first.to_string());
    }
    for l in &lines[start + 1..] {
        let t = l.trim();
        // Not the message: traces, source excerpts (`at …`, `  12|  …`), Nix's own hints.
        if t.is_empty() || t.starts_with('…') || t.starts_with("at ") || t.contains('|') && t.split('|').next().is_some_and(|n| n.trim().chars().all(|c| c.is_ascii_digit())) || t.starts_with("(use '--show-trace'") {
            continue;
        }
        parts.push(t.to_string());
    }
    let joined = parts.join(" ");
    let joined = joined.replace("Failed assertions: - ", "").replace("Failed assertions:", "");
    (!joined.trim().is_empty()).then(|| joined.trim().to_string())
}

/// The configuration's warnings (NixOS's `warnings`) in `stderr`, each one line: Nix writes each as
/// `evaluation warning: <text>` (older Nix: `trace: warning: <text>`), its later lines indented under
/// the first. Nix's own notes (`warning: Git tree … is dirty`) are not the configuration's.
pub fn warnings(stderr: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut open = false;
    for l in stderr.lines() {
        if let Some(w) = l.strip_prefix("evaluation warning: ").or_else(|| l.strip_prefix("trace: warning: ")) {
            found.push(w.trim().to_string());
            open = true;
        } else if let Some(last) = found.last_mut().filter(|_| open && l.starts_with(' ') && !l.trim().is_empty()) {
            last.push(' ');
            last.push_str(l.trim());
        } else {
            open = false;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configurations_warnings() {
        let stderr = "warning: Git tree '/x' is dirty\nevaluation warning: first warning\nevaluation warning: second\n                    with two lines\ntrace: warning: an older Nix's\nwarning: not the configuration's\n    nor this\n";
        assert_eq!(warnings(stderr), ["first warning", "second with two lines", "an older Nix's"]);
        assert!(warnings("").is_empty());
    }

    #[test]
    fn the_message_of_the_last_error() {
        // A failed assertion: the message is on the lines after a bare `error:`.
        let assertion = "error:\n       … while calling the 'head' builtin\n         at /nix/store/x-source/lib/attrsets.nix:1696:13:\n         1695|           if length values == 1 || pred here (elemAt values 1) (head values) then\n             |             ^\n       error:\n       Failed assertions:\n       - The ‘fileSystems’ option does not specify your root file system.\n";
        assert_eq!(message(assertion).as_deref(), Some("The ‘fileSystems’ option does not specify your root file system."));
        // A message on the `error:` line itself, and Nix's hint after it.
        let plain = "error: torq.packages: 'htpo' (user viper) is not a package in nixpkgs\n(use '--show-trace' to show detailed location information)\n";
        assert_eq!(message(plain).as_deref(), Some("torq.packages: 'htpo' (user viper) is not a package in nixpkgs"));
        // A wrong type: the definition values are part of the message.
        let typed = "error: A definition for option `users.users.alice.shell' is not of type `null or package or shell package'. Definition values:\n       - In `/nix/store/y-source/nixos/users.json': \"zsh\"\n";
        let m = message(typed).unwrap();
        assert!(m.starts_with("A definition for option `users.users.alice.shell' is not of type") && m.ends_with("\"zsh\""), "{m}");
        assert_eq!(message("warning: Git tree is dirty\n"), None, "no error");
        assert_eq!(message("error:\n"), None, "an error with no words");
    }
}
