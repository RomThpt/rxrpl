//! Integration tests for resume-from-disk (validator restart without RPC
//! re-bootstrap).
//!
//! These exercise the building blocks of `Node::resume_from_disk` directly
//! against a node store we populate by flushing real ledgers: the persisted
//! 118-byte header pointer (`resume_ledger_store::{save,load}`) plus lazy
//! reconstruction (`Ledger::from_header`). Sharing one `Arc<dyn NodeStore>`
//! across the "save" and "load" phases faithfully models a persistent store
//! surviving a process restart — no network and no RocksDB feature required.

use std::sync::Arc;

use rxrpl_ledger::Ledger;
use rxrpl_node::resume_ledger_store;
use rxrpl_nodestore::{CachedNodeStore, MemoryNodeDatabase};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::NodeStore;

/// A fresh in-memory node store (keyed by hash, like the persistent backend
/// but without a RocksDB feature gate).
fn build_store() -> Arc<dyn NodeStore> {
    Arc::new(CachedNodeStore::with_defaults(MemoryNodeDatabase::new()))
}

/// Close + flush a chain of two ledgers into `store`, planting a known state
/// entry in the validated ledger #2. Returns the validated (closed) ledger #2.
fn flush_two_ledgers(store: &Arc<dyn NodeStore>, known_key: Hash256, known_data: &[u8]) -> Ledger {
    // Ledger #1 (genesis), close + flush.
    let mut closed1 = Ledger::genesis_with_store(Arc::clone(store));
    closed1.close(100, 0).expect("close genesis");
    closed1.flush().expect("flush genesis");

    // Ledger #2: open on #1, plant a known state entry, close + flush.
    let mut open2 = Ledger::new_open(&closed1);
    open2
        .put_state(known_key, known_data.to_vec())
        .expect("put_state");
    open2.close(110, 0).expect("close #2");
    open2.flush().expect("flush #2");
    open2
}

#[test]
fn resume_reconstructs_validated_ledger_from_store() {
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path();
    let store = build_store();

    let known_key = Hash256::new([0xAB; 32]);
    let known_data = b"hello-resume-state".to_vec();

    // ---- Run #1: close two ledgers, persist nodes + the resume pointer. ----
    let validated = flush_two_ledgers(&store, known_key, &known_data);
    let expected_seq = validated.header.sequence;
    let expected_hash = validated.header.hash;
    assert_eq!(expected_seq, 2, "validated ledger should be #2");
    resume_ledger_store::save(db_dir, &validated.header).expect("save resume pointer");

    // ---- Run #2 (simulated restart): same store + db_dir, no ledger #1/#2
    // in memory — only what was flushed survives. This is exactly the
    // sequence `Node::resume_from_disk` performs. ----
    let header = resume_ledger_store::load(db_dir).expect("resume pointer present");
    assert_eq!(header.sequence, expected_seq);
    assert_eq!(
        header.hash, expected_hash,
        "hash recomputed on load matches"
    );

    let resumed = Ledger::from_header(header, Arc::clone(&store))
        .expect("validated state reconstructable from store");
    // The reconstructed validated ledger is byte-identical (same hash).
    assert_eq!(resumed.header.hash, expected_hash);
    assert_eq!(resumed.header.sequence, expected_seq);
    // The known state entry is readable from the lazily-loaded SHAMap (its
    // leaf is fetched from the store on demand, not held in memory).
    assert_eq!(
        resumed.get_state(&known_key),
        Some(known_data.as_slice()),
        "known state entry lazily readable after resume"
    );

    // The new open ledger's parent is the persisted validated ledger.
    let open = Ledger::new_open(&resumed);
    assert_eq!(
        open.header.sequence,
        expected_seq + 1,
        "open ledger = validated + 1"
    );
    assert_eq!(
        open.header.parent_hash, expected_hash,
        "open ledger parent is the resumed validated ledger"
    );
}

#[test]
fn resume_falls_through_when_state_nodes_missing() {
    // A resume pointer whose account_hash subtree is absent from the store
    // must NOT reconstruct — `from_header` errors and the caller falls back
    // to the RPC/genesis path. This guards the "best-effort" contract.
    let dir = tempfile::tempdir().unwrap();
    let db_dir = dir.path();
    let empty_store = build_store();

    // Forge a header that references a non-zero account/tx root that was
    // never flushed to this store.
    let mut header = rxrpl_ledger::LedgerHeader::new();
    header.sequence = 42;
    header.account_hash = Hash256::new([0x77; 32]);
    header.tx_hash = Hash256::ZERO;
    header.hash = header.compute_hash();
    resume_ledger_store::save(db_dir, &header).expect("save");

    let loaded = resume_ledger_store::load(db_dir).expect("pointer present");
    assert_eq!(loaded.sequence, 42);
    // Missing state root → reconstruction fails → resume would return false.
    assert!(
        Ledger::from_header(loaded, Arc::clone(&empty_store)).is_err(),
        "from_header must fail when the state subtree is missing"
    );
}

#[test]
fn resume_pointer_absent_is_none() {
    // No prior run: nothing to resume from.
    let dir = tempfile::tempdir().unwrap();
    assert!(resume_ledger_store::load(dir.path()).is_none());
}
