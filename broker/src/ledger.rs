//! Append-only, hash-chained ledger (JSON Lines).
//!
//! hash = sha256(prev_hash || canonical_json(body)), where `body` is the entry without `hash`.
//! serde_json serialises struct fields in declaration order and `Value` maps as sorted
//! BTreeMaps (we do not enable `preserve_order`), so the encoding is deterministic.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const GENESIS_PREV: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The hashed part of an entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Body {
    pub seq: u64,
    pub ts: String,
    pub actor: String,
    pub approved_by: Option<String>,
    pub kind: String,
    pub tier: String,
    pub tainted: bool,
    pub reason: String,
    pub data: Value,
    pub prev_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    #[serde(flatten)]
    pub body: Body,
    pub hash: String,
}

pub fn hash_body(body: &Body) -> Result<String> {
    let json = serde_json::to_vec(body)?;
    let mut h = Sha256::new();
    h.update(body.prev_hash.as_bytes());
    h.update(&json);
    Ok(hex::encode(h.finalize()))
}

/// Fields supplied by the caller; seq/ts/prev_hash/hash are filled in by the ledger.
pub struct NewEntry {
    pub actor: String,
    pub approved_by: Option<String>,
    pub kind: String,
    pub tier: String,
    pub tainted: bool,
    pub reason: String,
    pub data: Value,
}

pub struct Ledger {
    path: PathBuf,
    /// How the request came (`socket`, `sudo`): noted in every entry this broker appends (ADR-040).
    via: Option<String>,
    /// Who asked, as the kernel said (`socket` only): noted with it.
    caller: Option<String>,
}

impl Ledger {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), via: None, caller: None }
    }

    /// Note in every entry appended how the request came: what the broker was told, and, for
    /// `sudo`, what root says about itself (ADR-040).
    pub fn set_via(&mut self, via: Option<String>, caller: Option<String>) {
        self.via = via;
        self.caller = caller;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read_all(&self) -> Result<Vec<Entry>> {
        // Parsed after the guard is gone: readers hold writers off for the read alone.
        let text = {
            let _held = self.shared();
            self.text()?
        };
        Self::parse(&text)
    }

    /// Hold off writers while the guard lives (a line is never seen half written). For a moment only,
    /// never across the network: `append` waits for it. None: nothing has been written yet.
    pub fn shared(&self) -> Option<fs::File> {
        let f = fs::File::open(self.path.with_extension("lock")).ok()?;
        f.lock_shared().ok()?;
        Some(f)
    }

    /// The ledger's whole lines. A last line with no newline is, almost always, an append a power cut or a full disk
    /// stopped: every line is written whole, newline last, and synced before its operation goes on,
    /// so that operation did not finish. It stays out of what is read,
    /// and is settled at the next `append` or `mend` (`mend_locked` says how). A whole line that does not verify
    /// is another thing, an edit, and is never mended.
    pub fn text(&self) -> Result<String> {
        Ok(self.split()?.0)
    }

    /// The whole lines, and the torn tail after them.
    fn split(&self) -> Result<(String, Vec<u8>)> {
        if !self.path.exists() {
            return Ok((String::new(), Vec::new()));
        }
        let mut bytes = fs::read(&self.path).with_context(|| format!("opening ledger {}", self.path.display()))?;
        let keep = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let tail = bytes.split_off(keep);
        Ok((String::from_utf8(bytes).context("the ledger is not text")?, tail))
    }

    /// A torn last line gone, and an entry to say so, its bytes in it. What pushes the ledger calls
    /// this first: a remote that has seen a torn line would never match again. None: nothing to say.
    pub fn mend(&self) -> Result<Option<Entry>> {
        let _guard = self.exclusive()?;
        self.mend_locked()
    }

    /// Whether a torn line is waiting, for `status`: nothing mends it until something writes.
    pub fn torn(&self) -> bool {
        let _held = self.shared();
        self.split().is_ok_and(|(_, tail)| !tail.is_empty())
    }

    /// A whole ledger from elsewhere and the entry that follows it, at once (a machine adopting
    /// another's): both or neither, so a machine is never the other's with no record of becoming it.
    pub fn install(&self, bytes: &[u8], next: NewEntry) -> Result<Entry> {
        let _guard = self.exclusive()?;
        let text = std::str::from_utf8(bytes).context("the ledger is not text")?;
        let entries = Self::parse(text)?;
        verify_entries(&entries)?;
        let entry = self.next_entry(&entries, next)?;
        let mut all = bytes.to_vec();
        all.extend_from_slice(format!("{}\n", serde_json::to_string(&entry)?).as_bytes());
        self.write_durably(&self.path, &all)?;
        Ok(entry)
    }

    fn mend_locked(&self) -> Result<Option<Entry>> {
        const KEPT: usize = 8192;
        let (text, tail) = self.split()?;
        if tail.is_empty() {
            return Ok(None);
        }
        // A ledger edited is left as it was found, for a person to look at, tail and all.
        let entries = Self::parse(&text)?;
        verify_entries(&entries)?;
        // A whole entry, chained to the last, that lost its newline: it stands. Only what the hash
        // cannot vouch for goes.
        let whole = std::str::from_utf8(&tail).ok().and_then(|line| Self::parse(line).ok());
        if let Some(line) = whole.filter(|l| l.len() == 1) {
            if verify_entries(&[entries.as_slice(), line.as_slice()].concat()).is_ok() {
                let mut f = OpenOptions::new().append(true).open(&self.path)?;
                f.write_all(b"\n")?;
                f.sync_all()?;
                return Ok(None);
            }
        }
        // Nothing yet but this: the machine's first entry must be its first, or setup takes it for
        // set up. The bytes are kept beside the ledger, and it starts again.
        if entries.is_empty() {
            let kept = self.path.with_extension(format!("torn-{}", chrono::Utc::now().timestamp_millis()));
            self.write_durably(&kept, &tail)?;
            self.write_durably(&self.path, b"")?;
            return Ok(None);
        }
        let entry = self.next_entry(
            &entries,
            NewEntry {
                actor: "broker".into(),
                approved_by: None,
                kind: "ledger_mended".into(),
                tier: "system".into(),
                tainted: false,
                reason: format!("The ledger's last line was cut short ({} bytes), and removed", tail.len()),
                data: serde_json::json!({ "bytes": tail.len(), "sha256": hex::encode(Sha256::digest(&tail)), "tail": hex::encode(&tail[..tail.len().min(KEPT)]) }),
            },
        )?;
        // At once: the ledger as it was, or mended with its record. Never between, on a full disk least of all.
        let mut bytes = text.into_bytes();
        bytes.extend_from_slice(format!("{}\n", serde_json::to_string(&entry)?).as_bytes());
        self.write_durably(&self.path, &bytes)?;
        Ok(Some(entry))
    }

    /// `to`, holding `bytes` or as it was: a new file, synced, renamed over it, and the folder synced.
    fn write_durably(&self, to: &Path, bytes: &[u8]) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        let part = to.with_extension("new");
        let _ = fs::remove_file(&part);
        let written = (|| -> Result<()> {
            let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&part)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            fs::rename(&part, to)?;
            Ok(())
        })();
        if written.is_err() {
            let _ = fs::remove_file(&part);
        }
        written.with_context(|| format!("writing {}", to.display()))?;
        if let Some(dir) = to.parent() {
            fs::File::open(dir)?.sync_all()?;
        }
        Ok(())
    }

    fn exclusive(&self) -> Result<fs::File> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let guard = OpenOptions::new().write(true).create(true).truncate(false).open(self.path.with_extension("lock"))?;
        guard.lock().context("locking the ledger")?;
        Ok(guard)
    }


    fn parse(text: &str) -> Result<Vec<Entry>> {
        let mut out = vec![];
        for (i, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let e: Entry = serde_json::from_str(line).with_context(|| format!("ledger line {} is not a valid entry", i + 1))?;
            // Byte for byte what the broker writes for this entry (`append`): a field it does not know
            // (which the hash would not cover, as it hashes the entry it reads), a key twice, a value
            // written another way — any of them is an edit (audits of 2026-10-31).
            if serde_json::to_string(&e)? != line {
                bail!("ledger broken at line {}: entry contents were modified (it is not as the broker writes it)", i + 1);
            }
            out.push(e);
        }
        Ok(out)
    }

    pub fn get(&self, seq: u64) -> Result<Entry> {
        self.read_all()?
            .into_iter()
            .find(|e| e.body.seq == seq)
            .with_context(|| format!("no ledger entry with seq {seq}"))
    }

    /// Verify the full chain. Refuses to append on top of a broken chain.
    pub fn append(&self, new: NewEntry) -> Result<Entry> {
        // One writer at a time, and for milliseconds: separate from the broker's lock, which an
        // operation holds for minutes, so that `torq break-glass` is recorded while a switch is stuck.
        // A torn line goes first: a power cut must not cost the ledger, and every way back with it.
        let _guard = self.exclusive()?;
        self.mend_locked()?;
        self.append_locked(new)
    }

    fn append_locked(&self, new: NewEntry) -> Result<Entry> {
        let entries = Self::parse(&self.text()?)?;
        verify_entries(&entries)?;
        let entry = self.next_entry(&entries, new)?;
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(format!("{}\n", serde_json::to_string(&entry)?).as_bytes())?;
        f.sync_all()?;
        Ok(entry)
    }

    /// The entry that follows `entries`.
    fn next_entry(&self, entries: &[Entry], new: NewEntry) -> Result<Entry> {
        let (seq, prev_hash) = match entries.last() {
            Some(last) => (last.body.seq + 1, last.hash.clone()),
            None => (1, GENESIS_PREV.to_string()),
        };
        let mut data = new.data;
        if let Some(d) = data.as_object_mut() {
            for (k, v) in [("via", &self.via), ("caller", &self.caller)] {
                if let Some(v) = v {
                    d.insert(k.into(), serde_json::Value::String(v.clone()));
                }
            }
        }
        let body = Body {
            seq,
            ts: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            actor: new.actor,
            approved_by: new.approved_by,
            kind: new.kind,
            tier: new.tier,
            tainted: new.tainted,
            reason: new.reason,
            data,
            prev_hash,
        };
        let hash = hash_body(&body)?;
        Ok(Entry { body, hash })
    }

    pub fn verify(&self) -> Result<usize> {
        let entries = self.read_all()?;
        verify_entries(&entries)?;
        Ok(entries.len())
    }
}

pub fn verify_entries(entries: &[Entry]) -> Result<()> {
    let mut prev = GENESIS_PREV.to_string();
    for (i, e) in entries.iter().enumerate() {
        let expected_seq = i as u64 + 1;
        if e.body.seq != expected_seq {
            bail!("ledger broken at line {}: seq {} (expected {expected_seq})", i + 1, e.body.seq);
        }
        if e.body.prev_hash != prev {
            bail!("ledger broken at seq {}: prev_hash does not match previous entry", e.body.seq);
        }
        let h = hash_body(&e.body)?;
        if h != e.hash {
            bail!("ledger broken at seq {}: entry contents were modified", e.body.seq);
        }
        prev = e.hash.clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn new(reason: &str) -> NewEntry {
        NewEntry {
            actor: "agent".into(),
            approved_by: Some("casey".into()),
            kind: "commit".into(),
            tier: "user".into(),
            tainted: false,
            reason: reason.into(),
            data: json!({"b": 2, "a": 1}),
        }
    }

    #[test]
    fn chain_appends_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::open(dir.path().join("ledger.jsonl"));
        let a = l.append(new("one")).unwrap();
        let b = l.append(new("two")).unwrap();
        assert_eq!(a.body.prev_hash, GENESIS_PREV);
        assert_eq!(b.body.prev_hash, a.hash);
        assert_eq!(l.verify().unwrap(), 2);
    }

    /// ADR-040: how a request came is in each entry, inside what the hash covers.
    #[test]
    fn each_entry_says_how_the_request_came() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Ledger::open(dir.path().join("ledger.jsonl"));
        let plain = l.append(new("from a unit")).unwrap();
        assert!(plain.body.data.get("via").is_none());
        l.set_via(Some("sudo".into()), None);
        let e = l.append(new("through sudo")).unwrap();
        assert_eq!(e.body.data["via"], "sudo");
        assert_eq!(e.body.data["a"], 1, "the entry's own data is kept");
        assert_eq!(l.verify().unwrap(), 2);
        let p = dir.path().join("ledger.jsonl");
        fs::write(&p, fs::read_to_string(&p).unwrap().replace("\"via\":\"sudo\"", "\"via\":\"socket\"")).unwrap();
        assert!(l.verify().is_err(), "the note is covered by the hash");
    }

    #[test]
    fn detects_edited_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p);
        l.append(new("night light at sunset")).unwrap();
        l.append(new("second")).unwrap();
        let text = fs::read_to_string(&p).unwrap();
        fs::write(&p, text.replace("night light at sunset", "nothing to see here")).unwrap();
        let err = l.verify().unwrap_err().to_string();
        assert!(err.contains("seq 1"), "{err}");
        assert!(l.append(new("third")).is_err(), "must refuse to append to a broken chain");
    }

    /// The chain itself (the audit of M3, M6): an entry edited and its own hash made right again is
    /// caught by the next one's `prev_hash`, which no longer names it.
    #[test]
    fn an_entry_rehashed_after_an_edit_breaks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p);
        for r in ["one", "two", "three"] {
            l.append(new(r)).unwrap();
        }
        let mut all = l.read_all().unwrap();
        all[1].body.reason = "edited".into();
        all[1].hash = hash_body(&all[1].body).unwrap();
        let text: String = all.iter().map(|e| format!("{}\n", serde_json::to_string(e).unwrap())).collect();
        fs::write(&p, text).unwrap();
        let err = format!("{:#}", l.verify().unwrap_err());
        assert!(err.contains("seq 3") && err.contains("prev_hash"), "{err}");
    }

    /// An edit is never mended, even one that parses: an entry rehashed wrong, with a torn line after.
    #[test]
    fn an_edit_that_parses_is_not_mended() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p);
        l.append(new("one")).unwrap();
        l.append(new("two")).unwrap();
        let text = fs::read_to_string(&p).unwrap().replace("\"two\"", "\"TWO\"") + "{\"seq\":3,\"cut";
        fs::write(&p, &text).unwrap();
        assert!(l.mend().is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), text, "left as it was found");
    }

    /// From the audit of 2026-10-31: a field added to an entry (not one the hash covers, as it hashed
    /// only what it read) was dropped on reading, so the entry verified. Any field, anywhere.
    #[test]
    fn detects_an_added_field() {
        for (from, to) in [
            ("{\"seq\"", "{\"approved_by_agent\":\"forged\",\"seq\""),
            ("\"hash\"", "\"note\":1,\"hash\""),
            ("\"tainted\":false", "\"tainted\":0"),
            // A key twice: a reader keeping the first would show the forged one.
            ("\"reason\":\"a\"", "\"reason\":\"forged\",\"reason\":\"a\""),
            // The same entry, written another way.
            ("\"seq\":1,", "\"seq\": 1,"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("ledger.jsonl");
            let l = Ledger::open(&p);
            l.append(new("a")).unwrap();
            l.append(new("b")).unwrap();
            let text = fs::read_to_string(&p).unwrap();
            assert!(text.contains(from), "{from}");
            fs::write(&p, text.replacen(from, to, 1)).unwrap();
            let err = format!("{:#}", l.verify().unwrap_err());
            assert!(err.contains("line 1") || err.contains("not a valid entry"), "{to}: {err}");
            assert!(l.append(new("c")).is_err(), "{to}: must refuse to append to it");
        }
    }

    #[test]
    fn detects_deleted_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ledger.jsonl");
        let l = Ledger::open(&p);
        for r in ["a", "b", "c"] {
            l.append(new(r)).unwrap();
        }
        let lines: Vec<_> = fs::read_to_string(&p).unwrap().lines().map(String::from).collect();
        fs::write(&p, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(l.verify().is_err());
    }
}
