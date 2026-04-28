//! T35: integration coverage for the seq-cursor semantics that rippled
//! exposes via `LedgerTrie::getPreferred(largestSeq)`. rxrpl applies the
//! cursor at the ValidationsTrie layer (see DESIGN note in
//! `ledger_trie.rs`) so these scenarios live as integration tests against
//! the aggregator rather than as unit tests on the bare trie.

use rxrpl_consensus::types::{NodeId, Validation};
use rxrpl_consensus::validations_trie::ValidationsTrie;
use rxrpl_primitives::Hash256;

fn node(id: u8) -> NodeId {
    NodeId(Hash256::new([id; 32]))
}

fn h(byte: u8) -> Hash256 {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    Hash256::new(bytes)
}

fn validation(node_byte: u8, hash_byte: u8, seq: u32) -> Validation {
    Validation {
        node_id: node(node_byte),
        public_key: vec![],
        ledger_hash: h(hash_byte),
        ledger_seq: seq,
        full: true,
        // sign_time = seq * 100 keeps it monotone with seq so the
        // newer-vote tiebreak in `add_with_parents` stays predictable.
        close_time: seq * 100,
        sign_time: seq * 100,
        signature: None,
        amendments: vec![],
        signing_payload: None,
        ..Default::default()
    }
}

/// Mirrors the rippled `getPreferred(largestSeq)` "support older than the
/// cursor must be excluded" contract. Two trusted validators sit on
/// distinct ledgers; the older vote must drop out once `current_seq` rises
/// past it, leaving only the fresher tip eligible for the preferred answer.
#[test]
fn largest_seq_subtraction_excludes_support_strictly_below_cursor() {
    let mut agg = ValidationsTrie::new();
    agg.add_trusted(node(1));
    agg.add_trusted(node(2));

    // node 1 votes at seq 5; node 2 votes at seq 10.
    assert!(agg.add(validation(1, 0xAA, 5)));
    assert!(agg.add(validation(2, 0xBB, 10)));

    // Cursor at 5 -> both fresh -> tie (1 vs 1) -> higher hash wins.
    assert_eq!(agg.get_preferred(5), Some(h(0xBB)));
    // Cursor at 10 -> node 1 is stale -> only 0xBB participates.
    assert_eq!(agg.get_preferred(10), Some(h(0xBB)));
    // Cursor at 11 -> every cached vote is stale -> None.
    assert_eq!(agg.get_preferred(11), None);
}

/// Sibling-spans-with-different-seq case: two cohorts of trusted
/// validators sit on sibling tips that share an ancestor. Cohort A has
/// MORE total votes but at an OLDER seq; cohort B has FEWER votes at a
/// FRESHER seq. With `current_seq` set above cohort A's seq, A drops out
/// entirely and the preferred answer flips to cohort B's tip even though
/// cohort B is the structural minority. This is the exact semantics that
/// rippled's `getPreferred(largestSeq)` implements via per-node
/// `seqSupport` subtraction; rxrpl achieves it via the rebuild path in
/// `ValidationsTrie::get_preferred`.
#[test]
fn sibling_branches_with_different_seq_prefer_fresher_side_after_cursor_advance() {
    let mut agg = ValidationsTrie::new();
    for id in 1u8..=5 {
        agg.add_trusted(node(id));
    }

    // Cohort A (3 validators) votes for hash 0x10 at seq 5.
    // Cohort B (2 validators) votes for hash 0x20 at seq 9.
    assert!(agg.add(validation(1, 0x10, 5)));
    assert!(agg.add(validation(2, 0x10, 5)));
    assert!(agg.add(validation(3, 0x10, 5)));
    assert!(agg.add(validation(4, 0x20, 9)));
    assert!(agg.add(validation(5, 0x20, 9)));

    // Cursor at 5 -> all 5 fresh -> 0x10 wins (3 > 2).
    assert_eq!(agg.get_preferred(5), Some(h(0x10)));

    // Cursor at 9 -> cohort A stale -> only cohort B participates -> 0x20.
    assert_eq!(agg.get_preferred(9), Some(h(0x20)));

    // Cursor at 10 -> everything stale -> None.
    assert_eq!(agg.get_preferred(10), None);
}
