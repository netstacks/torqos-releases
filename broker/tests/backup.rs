//! The declared backup (ADR-038): its key kept outside the repository and the ledger, its list read
//! only, and a restore that is a snapshot's copy — restic stubbed by a script that says what it was
//! asked and does what each test needs. The copy itself, as the owner, needs root: tests/backup.nix.

// No unsafe code and no dead code, ever (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use torq_broker::Broker;

const ID: &str = "4bd1d0c5b0a5e3a3e1d2f0b8c9a7e6d5c4b3a2918f7e6d5c4b3a2918f7e6d5c4";
/// What the machine's restore runs, without systemd: restic itself.
const RESTORE: &str = "{restic} restore {snapshot} --target {room} --include {path} --sparse";

fn me() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The machine's accounts: alice and torq-test-nobody are this test's own uid, carol another's, rooty
/// root's, and the agent a fourth.
fn accounts() -> String {
    format!("alice:x:{0}:100::/home/alice:/bin/sh\ntorq-test-nobody:x:{0}:100::/:/bin/sh\ncarol:x:4242:100::/home/carol:/bin/sh\nrooty:x:0:0::/:/bin/sh\ntorq-agent:x:4343:4343::/var/empty:/bin/false\n", me())
}

/// A machine with one declared backup, `home`, whose restic is `script` (a shell script's body: `$@`
/// are restic's arguments, `$LOG` where they are written), with `passwd` its accounts.
struct Machine {
    dir: tempfile::TempDir,
    b: Broker,
}

impl Machine {
    fn new(script: &str) -> Self {
        Self::with_accounts(script, &accounts())
    }
    fn with_accounts(script: &str, passwd: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("config");
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("desktop.nix"), "{ }\n").unwrap();
        let restic = dir.path().join("restic-home");
        fs::write(&restic, format!("#!/bin/sh\nLOG={}\necho \"$@\" >> \"$LOG\"\n{script}\n", dir.path().join("asked").display())).unwrap();
        fs::set_permissions(&restic, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("passwd"), passwd).unwrap();
        let backups = BTreeMap::from([("home".to_string(), restic.to_string_lossy().into_owned())]);
        let b = Broker::new(&repo, dir.path().join("state"), "true")
            .with_backups(&backups)
            .with_backup_keys(dir.path().join("secrets/backup"))
            .with_backup_rooms(dir.path().join("cache/restoring"))
            .with_backup_restore(RESTORE)
            .with_backup_accounts(dir.path().join("passwd"));
        b.init().unwrap();
        Machine { dir, b }
    }
    fn keys(&self) -> PathBuf {
        self.dir.path().join("secrets/backup")
    }
    fn asked(&self) -> String {
        fs::read_to_string(self.dir.path().join("asked")).unwrap_or_default()
    }
    fn ledger(&self) -> String {
        fs::read_to_string(self.b.ledger.path()).unwrap()
    }
    fn room(&self) -> PathBuf {
        self.dir.path().join("cache/restoring")
    }
    fn rooms(&self) -> Vec<PathBuf> {
        fs::read_dir(self.room()).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default()
    }
    fn restore(&self, path: &str) -> String {
        format!("{:#}", self.b.restore_backup("home", None, path, "casey").unwrap_err())
    }
}

/// What restic says of the repository: one snapshot, `ID`; of what a path holds, one file of 3 bytes.
fn listing() -> String {
    format!(r#"case "$1 $2" in "snapshots "*) echo '[{{"id":"{ID}","short_id":"4bd1d0c5","time":"2026-11-07T01:00:00Z","hostname":"torq5","paths":["/home","/var/lib"]}}]' ;; "--no-lock ls") echo '{{"struct_type":"snapshot"}}'; echo '{{"struct_type":"node","type":"file","size":3}}' ;; esac"#)
}

/// A restic whose restore runs `body` (`$4` the room, `$6` the path), with a key given.
fn restoring(body: &str) -> Machine {
    let m = Machine::new(&format!("{}\n[ \"$1\" = restore ] && {{ {body}; }}\nexit 0", listing()));
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    m
}

#[test]
fn the_key_is_kept_root_s_alone_and_never_in_the_ledger() {
    let m = Machine::new("true");
    let secret = "correct horse battery staple";
    let e = m.b.set_backup_key("home", secret, "casey").unwrap();
    assert_eq!((e.body.kind.as_str(), e.body.approved_by.as_deref()), ("backup_key", Some("casey")));
    let file = m.keys().join("home");
    assert_eq!(fs::read_to_string(&file).unwrap(), secret, "as restic reads it: no newline");
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(m.keys()).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(!m.ledger().contains("horse"), "the key is never in the ledger");
    assert!(!fs::read_dir(m.keys()).unwrap().flatten().any(|f| f.file_name().to_string_lossy().ends_with(".new")), "nothing staged is left");
    // A new key replaces it, on the record too.
    m.b.set_backup_key("home", "another one", "casey").unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "another one");
    // A backup the configuration does not declare, or a key that is not one: refused, and nothing written.
    let n = m.b.ledger.read_all().unwrap().len();
    for (name, key) in [("work", "a key"), ("../passwords/casey", "a key"), ("home", ""), ("home", " spaced "), ("home", "two\nlines")] {
        assert!(m.b.set_backup_key(name, key, "casey").is_err(), "{name:?} {key:?}");
    }
    assert_eq!(m.b.ledger.read_all().unwrap().len(), n);
    assert!(!m.keys().join("work").exists());
    m.b.ledger.verify().unwrap();
}

#[test]
fn the_list_is_read_only_and_says_what_is_missing() {
    let m = Machine::new(&listing());
    let n = m.b.ledger.read_all().unwrap().len();
    let l = m.b.backups();
    assert_eq!((l.len(), l[0].key), (1, false));
    assert!(l[0].error.as_deref().unwrap().contains("request system backup key home"), "{:?}", l[0].error);
    assert!(m.asked().is_empty(), "no key: restic is not asked");
    m.b.set_backup_key("home", "the key", "casey").unwrap();
    let l = m.b.backups();
    assert_eq!((l[0].key, l[0].snapshots.len(), l[0].error.as_deref()), (true, 1, None));
    assert_eq!((l[0].snapshots[0].id.as_str(), l[0].snapshots[0].host.as_str()), (ID, "torq5"));
    assert_eq!(m.asked().trim(), "snapshots --json --no-lock");
    assert_eq!(m.b.ledger.read_all().unwrap().len(), n + 1, "the list writes nothing (the one entry is the key's)");
    // A repository that will not open: said, not a failure of the whole list.
    let bad = Machine::new("echo 'Fatal: wrong password or no key found' >&2; exit 1");
    bad.b.set_backup_key("home", "wrong", "casey").unwrap();
    let l = bad.b.backups();
    assert!(l[0].snapshots.is_empty() && l[0].error.as_deref().unwrap().contains("wrong password"), "{:?}", l[0].error);
}

#[test]
fn a_restore_names_what_it_may_and_asks_for_no_more() {
    let m = Machine::new(&listing());
    // Before restic is asked anything: a backup not declared, a snapshot that is not one, a path outside
    // the data or in the machine's own (the ledger, the configuration, the keys), or one restic would read
    // as a pattern.
    for (name, snap, path, why) in [
        ("work", None, "/home/alice", "declares no backup"),
        ("home", Some("HEAD~1"), "/home/alice", "not a snapshot"),
        ("home", None, "/etc/shadow", "below /home and /var/lib"),
        ("home", None, "/var/lib/torq/secrets/backup/home", "the machine's own"),
        ("home", None, "/var/lib/torq", "the machine's own"),
        ("home", None, "/home/alice/../../etc", "no `..`"),
        ("home", None, "/home/alice/*", "as a pattern"),
        ("home", None, "/home/alice/[ab]", "as a pattern"),
        ("home", None, "/home/alice", "no key here yet"),
    ] {
        let e = format!("{:#}", m.b.restore_backup(name, snap, path, "casey").unwrap_err());
        assert!(e.contains(why), "{path}: {e}");
    }
    assert!(m.asked().is_empty(), "restic was asked nothing: {}", m.asked());
    assert_eq!(m.b.ledger.read_all().unwrap().len(), 1, "refusals are not entries");
    let none = Machine::new("case \"$1\" in snapshots) echo '[]' ;; esac");
    none.b.set_backup_key("home", "k", "casey").unwrap();
    assert!(none.restore("/home/alice").contains("has no snapshot latest"));
}

/// restic fails: nothing live was touched, the room is gone, and the ledger says which snapshot, whole.
#[test]
fn a_failed_restore_is_ledgered_and_leaves_nothing() {
    let m = restoring("mkdir -p \"$4/home\"; echo 'Fatal: unable to open repository' >&2; exit 1");
    let e = m.restore("/home/alice/docs");
    assert!(e.contains("was not restored") && e.contains("unable to open repository"), "{e}");
    let asked = m.asked();
    let lines: Vec<&str> = asked.lines().collect();
    assert_eq!(lines[0], "snapshots --json --no-lock latest", "latest, resolved first: {asked}");
    assert_eq!(lines[1], format!("--no-lock ls --json --recursive {ID} /home/alice/docs"), "its size, before it is written: {asked}");
    assert!(lines[2].starts_with(&format!("restore {ID} --target ")) && lines[2].ends_with(" --include /home/alice/docs --sparse"), "{}", lines[2]);
    // Into the broker's own room, root's alone, not anywhere a person can reach.
    let target = lines[2].split(' ').nth(3).unwrap();
    assert!(Path::new(target).starts_with(m.room()), "{target}");
    assert!(m.rooms().is_empty(), "the room is gone: {:?}", m.rooms());
    let last = m.b.ledger.read_all().unwrap().pop().unwrap();
    assert_eq!((last.body.kind.as_str(), last.body.approved_by.as_deref(), last.body.tier.as_str()), ("backup_restored", Some("casey"), "system"));
    assert_eq!((last.body.data["backup_snapshot"].as_str(), last.body.data["path"].as_str()), (Some(ID), Some("/home/alice/docs")));
    assert!(last.body.reason.contains("it failed") && last.body.data["error"].as_str().unwrap().contains("unable to open"), "{:?}", last.body);
    assert!(!m.ledger().contains("the key"));
    m.b.ledger.verify().unwrap();
}

/// What restic brought back is checked as a snapshot is: a link on the way is refused, a FIFO is not
/// opened, and a folder whose live parent is not there is not made by root. The room goes.
#[test]
fn what_restic_brought_back_is_checked_as_a_snapshot_is() {
    let m = restoring("mkdir -p \"$4/home/torq-test-nobody\" && ln -s /etc \"$4/home/torq-test-nobody/docs\"");
    let e = format!("{:#}", m.b.restore_backup("home", Some("4bd1d0c5"), "/home/torq-test-nobody/docs/passwd", "casey").unwrap_err());
    assert!(e.contains("is a link"), "{e}");
    assert!(m.asked().lines().next().unwrap().ends_with("4bd1d0c5"), "the snapshot named: {}", m.asked());
    assert!(m.rooms().is_empty());
    let m = restoring("mkdir -p \"$4/home/torq-test-nobody\" && mkfifo \"$4$6\"");
    assert!(m.restore("/home/torq-test-nobody/pipe").contains("neither a file nor a folder"));
    let m = restoring("mkdir -p \"$4/home/torq-test-nobody\" && echo back > \"$4$6\"");
    assert!(m.restore("/home/torq-test-nobody/a").contains("is not there: restore that first"));
    assert!(!Path::new("/home/torq-test-nobody").exists());
    assert!(m.rooms().is_empty());
    assert_eq!(m.b.ledger.read_all().unwrap().pop().unwrap().body.kind, "backup_restored");
}

/// The review of the backup, H1: whose a folder is, the machine says, never the repository. A home is
/// its account's, which must exist; a service's folder its live owner's, which must be there; neither
/// root's nor the agent's; and the uid restic restored must be that one.
#[test]
fn whose_it_is_the_machine_says() {
    let m = restoring("mkdir -p \"$4$6\" && echo x > \"$4$6/f\"");
    assert!(m.restore("/home/nobody-here").contains("no account named nobody-here"));
    let e = m.restore("/home/carol");
    assert!(e.contains(&format!("uid {} in the backup and uid 4242 here", me())), "{e}");
    assert!(m.restore("/home/rooty").contains("root's or the agent's"));
    assert!(m.restore("/var/lib/torq-test-not-here").contains("is not on this machine"));
    // A service's folder of root's, live: root's, so not restored.
    if let Some(live) = ["/var/lib/systemd-coredump", "/var/lib/machines", "/var/lib/lastlog", "/var/lib/sudo"].into_iter().find(|p| fs::symlink_metadata(p).is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0)) {
        assert!(m.restore(live).contains("root's or the agent's"), "{live}");
    }
    // The agent's: refused, whatever its uid is.
    let agent = Machine::with_accounts(&format!("{}\n[ \"$1\" = restore ] && mkdir -p \"$4$6\"\nexit 0", listing()), &format!("torq-agent:x:{}:1::/var/empty:/bin/false\n", me()));
    agent.b.set_backup_key("home", "k", "casey").unwrap();
    assert!(agent.restore("/home/torq-agent").contains("root's or the agent's"));
    // None of it reached restic's restore for the account that is not here.
    assert!(m.rooms().is_empty());
}

/// M1: a restore that would not fit, with room to spare, is refused before anything is written.
#[test]
fn a_restore_that_would_not_fit_is_refused() {
    let m = Machine::new(r#"case "$1 $2" in "snapshots "*) echo '[{"id":"4bd1d0c5b0a5e3a3e1d2f0b8c9a7e6d5c4b3a2918f7e6d5c4b3a2918f7e6d5c4"}]' ;; "--no-lock ls") echo '{"struct_type":"node","type":"file","size":4611686018427387904}' ;; esac"#);
    m.b.set_backup_key("home", "k", "casey").unwrap();
    let e = m.restore("/home/alice/docs");
    assert!(e.contains("not restored: it takes"), "{e}");
    assert!(!m.asked().contains("restore "), "restic was not asked to write: {}", m.asked());
}

/// The room is the broker's own folder, closed, and so is the one it is in: a link, or anyone's, is
/// not written into, and nothing is mended to make it so (L1).
#[test]
fn the_room_is_the_broker_s_own() {
    let m = restoring("mkdir -p \"$4/home\"");
    let elsewhere = m.dir.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::DirBuilder::new().mode(0o700).create(m.dir.path().join("cache")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, m.room()).unwrap();
    assert!(m.restore("/home/alice").contains("not the broker's own folder"));
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    let open = restoring("mkdir -p \"$4/home\"");
    fs::create_dir_all(open.dir.path().join("cache")).unwrap();
    fs::set_permissions(open.dir.path().join("cache"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(open.restore("/home/alice").contains("closed to everyone else"));
    for m in [&m, &open] {
        assert!(!m.asked().contains("restore "), "restic was not asked to write: {}", m.asked());
    }
}

/// L1: what a restore cut short left goes at the next one, whatever became of its pid; a room still
/// held by a restore in progress stays.
#[test]
fn a_restore_cut_short_is_cleared_and_one_in_progress_is_not() {
    let m = restoring("mkdir -p \"$4/home\"");
    fs::DirBuilder::new().mode(0o700).recursive(true).create(m.room()).unwrap();
    for d in ["1-1", "2-2", "3-3"] {
        fs::create_dir(m.room().join(d)).unwrap();
        fs::write(m.room().join(d).join("secret"), "someone's").unwrap();
    }
    fs::write(m.room().join("1-1.lock"), "").unwrap();
    let held = fs::File::create(m.room().join("2-2.lock")).unwrap();
    held.lock().unwrap();
    fs::write(m.room().join(".4-4.taking"), "").unwrap();
    m.restore("/home/nobody-here");
    m.restore("/home/alice");
    let left: Vec<String> = m.rooms().iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left.contains(&"2-2".to_string()) && left.contains(&"2-2.lock".to_string()), "{left:?}");
}

/// The restore's command is the same in the module as in the broker (one is what a machine runs, the
/// other what a broker without its settings would).
#[test]
fn the_restore_command_is_one() {
    let Ok(nix) = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../modules/broker.nix")) else { return };
    let block = &nix[nix.find("backupRestoreCommand").unwrap()..];
    let block = &block[block.find('[').unwrap()..block.find("];").unwrap()];
    let joined: String = block.split('"').skip(1).step_by(2).collect();
    assert_eq!(joined, torq_broker::backup::RESTORE);
    for p in ["ProtectSystem=strict", "ReadWritePaths={room}", "PrivateDevices=yes", "NoNewPrivileges=yes", "--sparse"] {
        assert!(joined.contains(p), "{p}");
    }
    assert!(!joined.contains("CAP_MKNOD") && !joined.contains("CAP_SETUID"));
}
