//! Persistence for the **last validated ledger header** (resume-from-disk).
//!
//! Tiny binary sidecar at `<db_dir>/resume_ledger.bin`. Holds exactly the
//! canonical 118-byte ledger header (`LedgerHeader::to_raw_bytes`). The hash
//! is recomputed on load (`from_raw_bytes`), so no hash is stored.
//!
//! On every validated close the node overwrites this pointer immediately
//! after `Ledger::flush()` has persisted the SHAMap nodes to the node store.
//! On boot the node reads it and reconstructs the validated ledger lazily
//! (`Ledger::from_header`, which fetches the state/tx roots from the same
//! node store), skipping the ~32-minute RPC re-bootstrap.
//!
//! This is best-effort plumbing: a missing/short/corrupt file degrades to
//! "no resume pointer" and the caller falls through to the RPC/genesis path.
//! We own the file format end-to-end (raw header bytes), so there are no
//! schema-migration concerns.

use std::path::{Path, PathBuf};

use rxrpl_ledger::LedgerHeader;
use rxrpl_ledger::header::RAW_HEADER_SIZE;

const RESUME_FILE: &str = "resume_ledger.bin";
const RESUME_TMP: &str = "resume_ledger.tmp";

fn path_for(db_dir: &Path) -> PathBuf {
    db_dir.join(RESUME_FILE)
}

fn tmp_path_for(db_dir: &Path) -> PathBuf {
    db_dir.join(RESUME_TMP)
}

/// Persist the validated ledger header atomically.
///
/// Writes the 118 raw header bytes to `<db_dir>/resume_ledger.tmp` then
/// renames to `<db_dir>/resume_ledger.bin`, creating `db_dir` if needed.
/// The rename is atomic on the same filesystem, so a concurrent/crashing
/// reader never observes a partially written pointer.
pub fn save(db_dir: &Path, header: &LedgerHeader) -> std::io::Result<()> {
    std::fs::create_dir_all(db_dir)?;
    let tmp_path = tmp_path_for(db_dir);
    let final_path = path_for(db_dir);
    let bytes = header.to_raw_bytes();
    debug_assert_eq!(bytes.len(), RAW_HEADER_SIZE);
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

/// Read the persisted validated header, recomputing its hash.
///
/// Returns `None` on absence, short/long read, or decode failure — never
/// panics. The returned header has `hash` recomputed via `compute_hash()`,
/// so it is byte-identical to the one that was saved.
pub fn load(db_dir: &Path) -> Option<LedgerHeader> {
    let path = path_for(db_dir);
    let bytes = std::fs::read(&path).ok()?;
    LedgerHeader::from_raw_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn sample_header() -> LedgerHeader {
        let mut h = LedgerHeader::new();
        h.sequence = 1_234_567;
        h.drops = 99_999_999_000_000_000;
        h.parent_hash = Hash256::new([0x11; 32]);
        h.tx_hash = Hash256::new([0x22; 32]);
        h.account_hash = Hash256::new([0x33; 32]);
        h.parent_close_time = 700_000_001;
        h.close_time = 700_000_011;
        h.close_time_resolution = 30;
        h.close_flags = 0;
        h.hash = h.compute_hash();
        h
    }

    #[test]
    fn load_returns_none_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn save_then_load_roundtrip_all_fields_match() {
        let dir = tempfile::tempdir().unwrap();
        let header = sample_header();
        save(dir.path(), &header).unwrap();

        let loaded = load(dir.path()).expect("present after save");
        assert_eq!(loaded.sequence, header.sequence);
        assert_eq!(loaded.drops, header.drops);
        assert_eq!(loaded.parent_hash, header.parent_hash);
        assert_eq!(loaded.tx_hash, header.tx_hash);
        assert_eq!(loaded.account_hash, header.account_hash);
        assert_eq!(loaded.parent_close_time, header.parent_close_time);
        assert_eq!(loaded.close_time, header.close_time);
        assert_eq!(loaded.close_time_resolution, header.close_time_resolution);
        assert_eq!(loaded.close_flags, header.close_flags);
        // hash is recomputed on load and must match the saved one.
        assert_eq!(loaded.hash, header.hash);
    }

    #[test]
    fn save_overwrites_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let mut h1 = sample_header();
        h1.sequence = 100;
        h1.hash = h1.compute_hash();
        let mut h2 = sample_header();
        h2.sequence = 200;
        h2.hash = h2.compute_hash();

        save(dir.path(), &h1).unwrap();
        save(dir.path(), &h2).unwrap();

        let loaded = load(dir.path()).unwrap();
        assert_eq!(loaded.sequence, 200);
        assert_eq!(loaded.hash, h2.hash);
        assert!(
            !dir.path().join(RESUME_TMP).exists(),
            "no leftover .tmp after save"
        );
    }

    #[test]
    fn load_returns_none_on_short_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RESUME_FILE), b"too short").unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn load_returns_none_on_garbage_wrong_length() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RESUME_FILE), vec![0u8; RAW_HEADER_SIZE + 7]).unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn save_creates_db_dir_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        let header = sample_header();
        save(&nested, &header).unwrap();
        assert_eq!(load(&nested).unwrap().hash, header.hash);
    }
}
