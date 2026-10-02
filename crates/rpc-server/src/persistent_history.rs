//! Persistent ledger-header index for full-history nodes.
//!
//! SHAMap nodes are persisted by the node store, but a node also needs a
//! sequence-to-header index after restart in order to answer `ledger` and
//! `ledger_range` for ledgers older than the current resume pointer. Each
//! record is one canonical raw ledger header; a torn final record is ignored.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rxrpl_ledger::LedgerHeader;
use rxrpl_ledger::header::RAW_HEADER_SIZE;

const HEADER_FILE: &str = "ledger_headers.bin";

fn path_for(db_dir: &Path) -> PathBuf {
    db_dir.join(HEADER_FILE)
}

/// Append a closed ledger header to the persistent sequence index.
pub fn append_header(db_dir: &Path, header: &LedgerHeader) -> io::Result<()> {
    std::fs::create_dir_all(db_dir)?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path_for(db_dir))?;
    file.write_all(&header.to_raw_bytes())?;
    file.sync_data()
}

/// Find a persisted header by sequence number. The last matching record wins,
/// which also makes recovery from a duplicate append deterministic.
pub fn load_header(db_dir: &Path, sequence: u32) -> Option<LedgerHeader> {
    let mut file = std::fs::File::open(path_for(db_dir)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let mut found = None;
    for record in bytes.chunks_exact(RAW_HEADER_SIZE) {
        let Some(header) = LedgerHeader::from_raw_bytes(record) else {
            continue;
        };
        if header.sequence == sequence {
            found = Some(header);
        }
    }
    found
}

/// Return the first and last complete persisted sequence, if any.
pub fn load_range(db_dir: &Path) -> Option<(u32, u32)> {
    let mut file = std::fs::File::open(path_for(db_dir)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let mut range: Option<(u32, u32)> = None;
    for record in bytes.chunks_exact(RAW_HEADER_SIZE) {
        let Some(header) = LedgerHeader::from_raw_bytes(record) else {
            continue;
        };
        range = Some(match range {
            Some((min, max)) => (min.min(header.sequence), max.max(header.sequence)),
            None => (header.sequence, header.sequence),
        });
    }
    range
}
