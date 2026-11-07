//! `show system alarms`: is this machine as its history says — in words (docs/CLI-DESIGN.md), from the
//! broker's `status --json` facts, never its problem text. Pure, for snapshot tests.

use crate::history::one_line;
use crate::ui::Ui;
use serde_json::Value;

/// What `request system repair` can do about what was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    /// Nothing to do.
    Nothing,
    /// `--repair` returns the machine to its ledgered system (the broker's `repair`).
    Repair,
    /// A person has to look: the configuration was edited outside TorqOS, or the history is damaged.
    Person,
}

pub struct Report {
    pub text: String,
    pub exit: i32,
    pub fix: Fix,
}

/// `v`: the broker's `status --json`. `last`: the latest change, for the all-well line.
/// `repo`: the config repo, where a person would look. Each line a sentence, written to fit `ui`'s
/// window (ui.rs).
pub fn render(v: &Value, last: Option<&str>, repo: &str, ui: &Ui) -> Report {
    if v["busy"].is_string() {
        return Report {
            text: format!("{}\n", ui.line(&format!("  {} ", ui.dim("⋯")), "A change is being applied right now. Check again when it is done.")),
            exit: 75,
            fix: Fix::Nothing,
        };
    }
    let s = |k: &str| v[k].as_str();
    let mut found: Vec<(String, Fix)> = Vec::new();
    if s("ledger_error").is_some() {
        found.push(("The history (ledger) is damaged, so nothing can be checked or changed.".into(), Fix::Person));
    }
    if let Some(i) = v["interrupted"].as_object() {
        let op = one_line(i.get("op").and_then(|o| o.as_str()).unwrap_or("change"), 20);
        let target = i.get("target").and_then(|t| t.as_u64()).map(|t| t.to_string()).unwrap_or_else(|| "?".into());
        found.push((format!("A change was interrupted (the {op} of change {target})."), Fix::Repair));
    }
    // An interrupted change leaves its own commit or half-written edits behind; finishing it
    // (what --repair does first) deals with those, so they are not separate findings then.
    if v["interrupted"].is_null() {
        match (s("head"), s("ledgered_commit")) {
            (None, Some(_)) => found.push(("The configuration can't be read.".into(), Fix::Person)),
            (Some(h), Some(l)) if h != l => found.push(("The configuration was changed outside TorqOS.".into(), Fix::Person)),
            _ => {}
        }
        if v["dirty"] == true {
            found.push(("The configuration has edits made outside TorqOS.".into(), Fix::Person));
        }
    }
    if v["unrestored"].is_string() {
        found.push(("A change failed, and the machine couldn't be put back exactly as it was.".into(), Fix::Repair));
    }
    if let Some(want) = s("ledgered_system") {
        if s("running_system") != Some(want) {
            found.push(("It is running a system TorqOS didn't build (switched outside TorqOS).".into(), Fix::Repair));
        }
        // During a commit confirmed the boot default is, rightly, still the system it rolls back to (as the
        // broker says; audit of 2026-10-31): that is not drift.
        let anchor = v["pending_confirm"]["anchor_system"].as_str();
        if s("next_boot").is_some_and(|n| n != want && Some(n) != anchor) {
            found.push(("The next boot would start a system TorqOS didn't build.".into(), Fix::Repair));
        }
    }

    if let Some(list) = v["store_modified"].as_array().filter(|l| !l.is_empty()) {
        found.push((format!("The system's files were changed after they were built ({} of them, in the store).", list.len()), Fix::Repair));
    }
    // The one the design most wants seen: this machine's history and its copy elsewhere disagree.
    if let Some(why) = v["remote_alarm"]["why"].as_str() {
        found.push((format!("This machine's history and the copy on its remote disagree: {} (show system remote; once looked into, request system remote clear).", one_line(why, 160)), Fix::Person));
    }
    // The record against itself: no rebuild puts these right, so neither is offered one.
    if let Some(list) = v["disagreements"].as_array().filter(|l| !l.is_empty()) {
        found.push((format!("The history and the configuration's commits disagree: {} (look into it, then: request system ledger acknowledge).", list.iter().filter_map(|d| d.as_str()).map(|d| one_line(d, 120)).collect::<Vec<_>>().join("; ")), Fix::Person));
    }
    if v["torn"] == true {
        found.push(("The history's last line was cut short, by a power cut perhaps: nothing was lost that had finished, and the next change removes it.".into(), Fix::Nothing));
    }

    // The broker knows of a problem this command cannot name (a newer broker, say): never all-well.
    if found.is_empty() && v["ok"] != true {
        found.push(("TorqOS found a problem this command can't describe: an administrator can read it with show system remote and show system ledger, or the journal of torq-status.".into(), Fix::Person));
    }
    if found.is_empty() {
        let text = if v["checked_running_system"] == true {
            let last = last.map(|l| format!("{}\n", ui.dim(&ui.line("    ", &format!("Last change: {}", one_line(l, usize::MAX)))))).unwrap_or_default();
            format!("{}\n{last}", ui.ok("This machine is exactly as its history says."))
        } else {
            format!(
                "{}\n{}\n",
                ui.ok("This machine is as its history says, as far as recorded."),
                ui.dim(&ui.line("    ", "(the running system is checked from the next change on)"))
            )
        };
        return Report { text, exit: 0, fix: Fix::Nothing };
    }
    let any = |want: Fix| found.iter().any(|(_, f)| *f == want);
    let fix = if any(Fix::Person) { Fix::Person } else if any(Fix::Repair) { Fix::Repair } else { Fix::Nothing };
    let mut text = format!("{}\n", ui.warn(if fix == Fix::Nothing { "Something to know about this machine's history." } else { "This machine is not as its history says." }));
    for (what, _) in &found {
        text.push_str(&format!("{}\n", ui.line("    · ", what)));
    }
    text.push_str(&match fix {
        Fix::Nothing => String::new(),
        Fix::Repair => format!("{}\n", ui.line("    ", &format!("To return it to its history:  request system repair   {}", ui.dim("(asks first)")))),
        _ => format!("{}\n", ui.line("    ", &format!("This needs a person: look at {} (request system repair won't touch it).", one_line(repo, 40)))),
    });
    Report { text, exit: 1, fix }
}

/// What is worth knowing about how this machine is reached, though nothing is wrong (docs/INSTALL.md,
/// "SSH"): from the running SSH server's own settings (`sshd_config`, `None` when there is none). Its
/// first `PasswordAuthentication` is the one that holds (as sshd reads it); none means its default,
/// yes.
pub fn notes(sshd_config: Option<&str>, ui: &Ui) -> String {
    let Some(config) = sshd_config else { return String::new() };
    let passwords = config
        .lines()
        .filter_map(|l| l.split_whitespace().collect::<Vec<_>>().get(..2).map(|kv| (kv[0].to_ascii_lowercase(), kv[1].to_ascii_lowercase())))
        .find(|(k, _)| k == "passwordauthentication")
        .is_none_or(|(_, v)| v == "yes");
    if !passwords {
        return String::new();
    }
    format!(
        "{}\n",
        ui.dim(&ui.line("    Note: ", "SSH accepts passwords as well as keys: anyone who can reach this machine can try one (repeated wrong guesses are blocked)."))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_note_when_ssh_takes_passwords() {
        let note = notes(Some("UsePAM yes\nPasswordAuthentication yes\nPermitRootLogin no\n"), &Ui::plain());
        assert!(note.starts_with("    Note: SSH accepts passwords as well as keys"), "{note}");
        assert!(notes(Some("passwordauthentication YES\n"), &Ui::plain()).contains("SSH accepts passwords"), "any case, as sshd reads it");
        assert!(notes(Some("UsePAM yes\n"), &Ui::plain()).contains("SSH accepts passwords"), "sshd's default is yes");
        assert_eq!(notes(Some("PasswordAuthentication no\nPasswordAuthentication yes\n"), &Ui::plain()), "", "the first holds");
        assert_eq!(notes(None, &Ui::plain()), "", "no SSH server");
        let narrow = notes(Some("PasswordAuthentication yes\n"), &Ui { fit: Some(30), ..Ui::plain() });
        assert!(narrow.lines().all(|l| crate::ui::width(l) <= 30), "{narrow}");
    }

    fn ok() -> Value {
        json!({ "ok": true, "busy": null, "head": "c1", "ledgered_commit": "c1", "dirty": false,
                "interrupted": null, "unrestored": null, "running_system": "/nix/store/a",
                "next_boot": "/nix/store/a", "ledgered_system": "/nix/store/a", "checked_running_system": true })
    }


    /// The record against itself: named, with what to do, and no rebuild offered for it.

    /// The remote's alarm is named where a person looks (the audit of M3, L1).
    #[test]
    fn the_remotes_alarm_is_named() {
        let mut v = ok();
        v["ok"] = json!(false);
        v["remote_alarm"] = json!({ "at": "2026-11-06T00:00:00Z", "why": "the remote's ledger was rewritten" });
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        let text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!((r.exit, r.fix), (1, Fix::Person));
        assert!(text.contains("copy on its remote disagree") && text.contains("rewritten") && text.contains("request system remote clear"), "{text}");
    }
    #[test]
    fn what_a_rebuild_does_not_touch() {
        let mut v = ok();
        v["ok"] = json!(false);
        v["torn"] = json!(true);
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Nothing));
        assert!(r.text.contains("cut short") && r.text.contains("next change") && !r.text.contains("repair") && !r.text.contains("not as its history"), "{}", r.text);
        let mut v = ok();
        v["ok"] = json!(false);
        v["disagreements"] = json!(["#7 says casey approved it, its commit says \"mallory\""]);
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Person));
        let text = r.text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains("#7 says casey") && text.contains("request system ledger acknowledge"), "{text}");
    }
    #[test]
    fn all_well() {
        let r = render(&ok(), Some("Install htop for viper"), "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (0, Fix::Nothing));
        assert_eq!(r.text, "  ✓ This machine is exactly as its history says.\n    Last change: Install htop for viper\n");
        let narrow = render(&ok(), Some("Install htop for viper"), "/var/lib/torq/config", &Ui { fit: Some(30), ..Ui::plain() });
        assert_eq!(narrow.text, "  ✓ This machine is exactly as\n    its history says.\n    Last change: Install htop\n    for viper\n", "a narrow terminal");
    }

    #[test]
    fn switched_outside_torq_is_repairable() {
        let mut v = ok();
        v["running_system"] = json!("/nix/store/stock-nixos");
        v["next_boot"] = json!("/nix/store/stock-nixos");
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Repair));
        let expected = "  ⚠ This machine is not as its history says.\n    · It is running a system TorqOS didn't build (switched outside TorqOS).\n    · The next boot would start a system TorqOS didn't build.\n    To return it to its history:  request system repair   (asks first)\n";
        assert_eq!(r.text, expected);
    }

    /// From the audit of 2026-10-31: during a commit confirmed, the boot default is the system it rolls
    /// back to — not drift (the broker says so too); any other boot default still is.
    #[test]
    fn a_commit_confirmed_waiting_is_not_boot_drift() {
        let mut v = ok();
        v["next_boot"] = json!("/nix/store/anchor");
        v["pending_confirm"] = json!({ "applied": 7, "anchor_system": "/nix/store/anchor", "deadline": "soon" });
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert!(!r.text.contains("next boot"), "{}", r.text);
        assert_ne!(r.fix, Fix::Repair, "repair is refused while it waits: never suggested");
        v["next_boot"] = json!("/nix/store/elsewhere");
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert!(r.text.contains("next boot would start"), "{}", r.text);
    }

    #[test]
    fn an_edited_configuration_needs_a_person() {
        let mut v = ok();
        v["dirty"] = json!(true);
        v["running_system"] = json!("/nix/store/other");
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Person), "repair would build the edit: a person decides");
        assert!(r.text.contains("needs a person"), "{}", r.text);
    }

    #[test]
    fn busy_and_unrecorded() {
        let r = render(&json!({ "ok": false, "busy": "123 (nixos-rebuild)", "problems": [] }), None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!((r.exit, r.fix), (75, Fix::Nothing));
        let mut v = ok();
        v["checked_running_system"] = json!(false);
        v["ledgered_system"] = json!(null);
        assert!(render(&v, None, "/var/lib/torq/config", &Ui::plain()).text.contains("as far as recorded"));
    }

    #[test]
    fn an_apply_interrupted_after_its_commit_is_still_repairable() {
        // The broker died after committing the change and before ledgering it: HEAD has moved and
        // the tree may be half-written, but repair reconciles that first.
        let mut v = ok();
        v["ok"] = json!(false);
        v["interrupted"] = json!({ "op": "apply", "target": 12 });
        v["head"] = json!("c2");
        v["dirty"] = json!(true);
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!(r.fix, Fix::Repair, "{}", r.text);
        assert!(!r.text.contains("outside TorqOS"), "{}", r.text);
    }

    #[test]
    fn a_problem_it_cannot_name_is_not_all_well() {
        let mut v = ok();
        v["ok"] = json!(false);
        let r = render(&v, None, "/srv/cfg", &Ui::plain());
        assert_eq!((r.exit, r.fix), (1, Fix::Person));
        assert!(r.text.contains("look at /srv/cfg"), "the repo it was given:\n{}", r.text);
    }

    #[test]
    fn interrupted_is_repairable() {
        let mut v = ok();
        v["interrupted"] = json!({ "op": "apply", "target": 12 });
        let r = render(&v, None, "/var/lib/torq/config", &Ui::plain());
        assert_eq!(r.fix, Fix::Repair);
        assert!(r.text.contains("A change was interrupted (the apply of change 12)."), "{}", r.text);
    }
}
