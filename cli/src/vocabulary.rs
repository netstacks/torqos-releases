//! Other CLIs' words (Junos, Cisco, a shell's) and where what they mean is here (ADR-025): one table,
//! read wherever such a word is refused — a command, a pipe, a setting's path — and by `help`, which
//! leads with the place a concept usually is.

/// A command or pipe word of another CLI, refused here, and what to use instead.
pub struct Word {
    /// Where it was typed: as a command, or after a `|`.
    pub at: At,
    pub word: &'static str,
    pub instead: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum At {
    Command,
    Pipe,
}

pub const WORDS: &[Word] = &[
    Word { at: At::Pipe, word: "count", instead: "lines are counted with `| wc -l`" },
    Word { at: At::Pipe, word: "match", instead: "lines are matched with `| grep <pattern>`" },
    Word { at: At::Pipe, word: "except", instead: "lines are left out with `| grep -v <pattern>`" },
    Word { at: At::Pipe, word: "find", instead: "lines are found with `| grep <pattern>`" },
    Word { at: At::Pipe, word: "last", instead: "the last lines are `| tail -n <lines>`" },
    Word { at: At::Pipe, word: "trim", instead: "columns are cut with `| cut -c <from>-`" },
    Word { at: At::Pipe, word: "save", instead: "the configuration is saved with `save <file>`, in configuration mode" },
    Word { at: At::Command, word: "copy", instead: "TorqOS has no copy: set the new place's values (`show <place> | display set` lists the old ones)" },
    Word { at: At::Command, word: "rename", instead: "TorqOS has no rename: set the new place, then delete the old one" },
    Word { at: At::Command, word: "annotate", instead: "a change's reason is recorded with `commit comment \"…\"`" },
    Word { at: At::Command, word: "deactivate", instead: "TorqOS has no inactive settings: delete it (its history keeps it)" },
    Word { at: At::Command, word: "ping", instead: "ping is a shell command: run it outside torqcli (`exit`, or another terminal)" },
    Word { at: At::Command, word: "traceroute", instead: "traceroute is a shell command: run it outside torqcli (`exit`, or another terminal)" },
    Word { at: At::Command, word: "ssh", instead: "ssh is a shell command: run it outside torqcli (`exit`, or another terminal)" },
];

/// What `word`, typed `at` a command or after a `|`, is instead, if it is another CLI's.
pub fn instead(at: At, word: &str) -> Option<&'static str> {
    WORDS.iter().find(|w| w.at == at && w.word == word).map(|w| w.instead)
}

/// A concept as other systems name it, and the place it usually is here.
pub struct Concept {
    /// The words people search for, or type as a path (`system services ssh`), in small letters.
    pub names: &'static [&'static str],
    /// Another CLI's path for it (its first words), refused here: said where it is instead. (None
    /// where that path works here as it is: `system host-name` is an old name kept.)
    pub path: Option<&'static [&'static str]>,
    /// The place here, and how it is set.
    pub place: &'static str,
    pub how: &'static str,
}

pub const CONCEPTS: &[Concept] = &[
    Concept { names: &["ssh", "sshd"], path: Some(&["system", "services", "ssh"]), place: "services openssh", how: "`set services openssh enable true` (keys only; passwords: `services openssh settings PasswordAuthentication true`)" },
    Concept { names: &["hostname", "host-name"], path: None, place: "networking hostName", how: "`set networking hostName <name>`" },
    Concept { names: &["dns", "nameserver", "name-server", "resolver"], path: Some(&["system", "name-server"]), place: "networking nameservers", how: "`set networking nameservers [ <address> … ]`" },
    Concept { names: &["ntp", "timeserver"], path: Some(&["system", "ntp"]), place: "networking timeServers", how: "`set networking timeServers [ <server> … ]`" },
    Concept { names: &["timezone", "time-zone"], path: None, place: "time timeZone", how: "`set time timeZone <zone>`" },
    Concept { names: &["user", "account", "login"], path: Some(&["system", "login"]), place: "users users", how: "`set users users <name> isNormalUser true` adds a person (`request system password <name>` sets their password)" },
    Concept { names: &["interface", "interfaces", "address", "ip"], path: Some(&["interfaces"]), place: "networking interfaces", how: "`set networking interfaces <name> ipv4 addresses 1 address <address>` (and `… 1 prefixLength <n>`)" },
    Concept { names: &["route", "routes", "static"], path: Some(&["routing-options"]), place: "networking interfaces <name> ipv4 routes", how: "`set networking interfaces <name> ipv4 routes 1 address <net>` (`… prefixLength`, `… via`); the default route: `networking defaultGateway`" },
    Concept { names: &["firewall", "filter"], path: Some(&["firewall"]), place: "networking firewall", how: "`set networking firewall allowedTCPPorts [ <port> … ]`" },
    Concept { names: &["package", "packages", "install", "software"], path: None, place: "environment systemPackages", how: "`set environment systemPackages <name>` (yours only: `users users <you> packages`); `show packages <word>` searches" },
    Concept { names: &["syslog", "log", "journal"], path: Some(&["system", "syslog"]), place: "services journald", how: "the log is systemd's journal (`services journald`); read it with `journalctl`, outside torqcli" },
];

/// The concept another CLI's path is, if `words` begin one (a path refused here).
pub fn concept_of_path(words: &[String]) -> Option<&'static Concept> {
    CONCEPTS.iter().find(|c| c.path.is_some_and(|p| words.len() >= p.len() && p.iter().zip(words).all(|(p, w)| p == w)))
}

/// The concepts searched for, in `terms` (small letters).
pub fn concepts_of(terms: &[String]) -> Vec<&'static Concept> {
    CONCEPTS.iter().filter(|c| terms.iter().any(|t| c.names.contains(&t.as_str()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_clis_words_and_where_they_are_here() {
        assert_eq!(instead(At::Pipe, "count"), Some("lines are counted with `| wc -l`"));
        assert_eq!(instead(At::Command, "count"), None, "a pipe word, not a command");
        let w = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(concept_of_path(&w("system services ssh")).map(|c| c.place), Some("services openssh"));
        assert_eq!(concept_of_path(&w("interfaces ge-0/0/0 unit 0")).map(|c| c.place), Some("networking interfaces"));
        assert!(concept_of_path(&w("services openssh")).is_none(), "a path here is not another CLI's");
        assert!(concept_of_path(&w("system host-name x")).is_none(), "an old name kept works as it is");
        assert_eq!(concepts_of(&["ssh".into()]).len(), 1);
    }
}
