//! The ledger's bytes never change (ADR-044, step 0). `wire/tests/fixtures/` holds ledgers the broker
//! wrote before its words moved into `torq-wire` — from its own tests: the trust loop, `commit at`,
//! backups, snapshots, passwords, session trust, a mended tear and break-glass, confirmed and expired
//! commits — and one written by hand with a kind, a tier and an actor no torq knows yet. Every line read
//! as an `Entry` and written again must be byte for byte the line, and every fixture must verify: a
//! change of a field's name, its order or its spelling would change a hash, and the record with it.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use std::fs;
use std::path::Path;
use torq_broker::ledger::{verify_entries, Entry};

#[test]
fn every_fixture_line_is_written_back_as_it_was_and_verifies() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../wire/tests/fixtures");
    let mut files: Vec<_> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    files.sort();
    assert!(files.len() >= 15, "the fixtures are missing: {}", dir.display());
    let mut kinds = std::collections::BTreeSet::new();
    for f in &files {
        let text = fs::read_to_string(f).unwrap();
        let entries: Vec<Entry> = text
            .lines()
            .map(|line| {
                let e: Entry = serde_json::from_str(line).unwrap_or_else(|err| panic!("{}: {err}: {line}", f.display()));
                assert_eq!(serde_json::to_string(&e).unwrap(), line, "{}: written back differently", f.display());
                kinds.insert(e.body.kind.clone());
                e
            })
            .collect();
        verify_entries(&entries).unwrap_or_else(|err| panic!("{}: {err:#}", f.display()));
    }
    // Every kind today's broker writes in its tests, and one it does not know.
    for k in ["init", "proposal", "applied", "apply_failed", "reverted", "revert_failed", "interrupted", "repaired",
              "confirmed", "rolled_back", "apply_scheduled", "apply_cancelled", "apply_refused", "refused",
              "backup_key", "backup_restored", "snapshot_taken", "snapshot_deleted", "snapshot_delete_failed",
              "password", "password_failed", "trust_granted", "trust_revoked", "trust_expired", "break_glass",
              "ledger_mended", "commit", "harvest_moon"] {
        assert!(kinds.contains(k), "no fixture has a {k} entry");
    }
}
