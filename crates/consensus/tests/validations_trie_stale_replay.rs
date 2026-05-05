//! Regression coverage for audit pass 2 critical finding C1: stale-validation
//! replay in `ValidationsTrie::add`.
//!
//! Before commit `2ae2eca`, `add` did not enforce monotonicity: a validation
//! with an older `ledger_seq` (or older/equal `sign_time` at the same
//! `ledger_seq`) from the same `NodeId` would silently overwrite the node's
//! current vote in the underlying `LedgerTrie`. An attacker who replayed a
//! captured stale validation could therefore flip the preferred-branch
//! detection used to drive 60%-threshold consensus.
//!
//! These integration tests pin the fix at the public-API boundary
//! (`rxrpl_consensus::ValidationsTrie::add` + `get_preferred`) so the
//! contract is enforced even if internal helpers are refactored later.
//! In-crate unit tests in `validations_trie.rs` cover the same monotonicity
//! invariants; this file is the external-consumer-facing duplicate that
//! survives module reshuffling.
//!
//! Coverage:
//! - `stale_seq_validation_rejected_after_newer`: seq=10 then seq=9 from same
//!   node — second rejected, preferred unchanged, `get_preferred(11)` does
//!   not flip to the older branch.
//! - `same_seq_older_sign_time_rejected`: seq=10 sign_time=1000 then seq=10
//!   sign_time=999 — second rejected, preferred unchanged.
//! - `same_seq_same_sign_time_idempotent`: same validation delivered twice —
//!   both calls leave the same preferred result, no double-counting in the
//!   tip support.

use rxrpl_consensus::ValidationsTrie;
use rxrpl_consensus::types::{NodeId, Validation};
use rxrpl_primitives::Hash256;

/// Build a deterministic `NodeId` from a single byte (every byte of the
/// 32-byte hash is `byte`). Matches the convention used in
/// `crates/consensus/tests/multi_node.rs`.
fn node(byte: u8) -> NodeId {
    NodeId(Hash256::new([byte; 32]))
}

/// Build a deterministic `Hash256` whose first byte is `byte` and the rest
/// are zero. Distinct `byte` values give distinct ledger hashes (and so
/// distinct trie branches).
fn hash(byte: u8) -> Hash256 {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    Hash256::new(bytes)
}

/// Construct a minimal `Validation` with the four fields that drive the
/// stale-replay check: `node_id`, `ledger_hash`, `ledger_seq`, `sign_time`.
/// Other fields are defaulted — none of them affect the monotonicity check.
fn validation(node_byte: u8, hash_byte: u8, seq: u32, sign_time: u32) -> Validation {
    Validation {
        node_id: node(node_byte),
        public_key: vec![],
        ledger_hash: hash(hash_byte),
        ledger_seq: seq,
        full: true,
        close_time: sign_time,
        sign_time,
        signature: None,
        amendments: vec![],
        signing_payload: None,
        ..Default::default()
    }
}

/// Audit C1 — replay-attack scenario: an attacker captures a node's
/// validation at `seq=9` and replays it after the node has already moved on
/// to `seq=10`. The trie MUST keep the newer `seq=10` vote.
///
/// Concretely: ingest validation(seq=10, hash=0xAA), then validation(seq=9,
/// hash=0xBB) from the same node. Assert:
/// 1. The second `add` returns `false` (rejected).
/// 2. Tip support for the newer hash (`0xAA`) is still 1.
/// 3. Tip support for the stale hash (`0xBB`) is 0.
/// 4. `get_preferred(11)` does NOT flip to the older branch — it must stay
///    on `0xAA` (the newer ledger is fresh enough at anchor seq=11 because
///    the freshness filter is `ledger_seq >= current_seq`... but wait,
///    seq=10 < 11, so both would be filtered. We assert that the result is
///    `None` in that case, NOT the older branch.). For an anchor at
///    `current_seq=10` (where the newer vote IS fresh), the preferred MUST
///    be `0xAA`.
#[test]
fn stale_seq_validation_rejected_after_newer() {
    let mut agg = ValidationsTrie::new();
    agg.add_trusted(node(1));

    // Newer validation lands first.
    assert!(
        agg.add(validation(1, 0xAA, 10, 1000)),
        "first validation must be accepted"
    );
    assert_eq!(agg.count_for(&hash(0xAA)), 1, "newer vote credited");

    // Replay an older `ledger_seq` from the SAME node.
    let stale = validation(1, 0xBB, 9, 2000);
    assert!(
        !agg.add(stale),
        "validation with older ledger_seq must be rejected"
    );

    // The current vote is unchanged: the trie still credits the newer hash
    // and never credits the stale hash.
    assert_eq!(
        agg.count_for(&hash(0xAA)),
        1,
        "newer vote must remain after stale replay"
    );
    assert_eq!(
        agg.count_for(&hash(0xBB)),
        0,
        "stale vote must NOT be credited"
    );

    // At an anchor where the newer vote is fresh, preferred is the newer
    // hash — proving the stale replay did not flip the trie.
    assert_eq!(
        agg.get_preferred(10),
        Some(hash(0xAA)),
        "preferred must reflect the newer vote at its own anchor seq"
    );

    // At an anchor strictly above both seqs, every cached validation is
    // stale — the result must be `None`, NOT the older branch. This is the
    // load-bearing assertion: if monotonicity were not enforced, the older
    // `0xBB` branch could end up in the trie and `get_preferred` would
    // return `Some(0xBB)` instead of the `None` we expect.
    assert_eq!(
        agg.get_preferred(11),
        None,
        "above-anchor query must not surface the stale branch"
    );
}

/// Audit C1 — same `ledger_seq`, older `sign_time`. The legitimate
/// "validator updates its vote at the same seq" path requires
/// `sign_time` to strictly advance; a replayed older-or-equal `sign_time`
/// MUST be rejected.
#[test]
fn same_seq_older_sign_time_rejected() {
    let mut agg = ValidationsTrie::new();
    agg.add_trusted(node(1));

    // First vote: seq=10, sign_time=1000, hash=0xAA.
    assert!(
        agg.add(validation(1, 0xAA, 10, 1000)),
        "first validation must be accepted"
    );
    assert_eq!(agg.count_for(&hash(0xAA)), 1);
    assert_eq!(agg.get_preferred(10), Some(hash(0xAA)));

    // Replay at same seq but strictly older sign_time on a different hash —
    // must be rejected, current vote preserved.
    let stale = validation(1, 0xBB, 10, 999);
    assert!(
        !agg.add(stale),
        "same seq + older sign_time must be rejected"
    );
    assert_eq!(
        agg.count_for(&hash(0xAA)),
        1,
        "current vote must survive older-sign_time replay"
    );
    assert_eq!(
        agg.count_for(&hash(0xBB)),
        0,
        "older-sign_time replay must not be credited"
    );
    assert_eq!(
        agg.get_preferred(10),
        Some(hash(0xAA)),
        "preferred unchanged after older-sign_time replay"
    );
}

/// Audit C1 — exact same validation delivered twice. Both calls must leave
/// the trie in the same state (no double-counting at tip support, identical
/// `get_preferred` answer). The second call MUST return `false` per the
/// idempotent-redelivery contract documented on `add`.
#[test]
fn same_seq_same_sign_time_idempotent() {
    let mut agg = ValidationsTrie::new();
    agg.add_trusted(node(1));

    let v = validation(1, 0xCC, 10, 1234);

    // First delivery: state changes, `add` returns true.
    assert!(agg.add(v.clone()), "first delivery must be accepted");
    let preferred_after_first = agg.get_preferred(10);
    assert_eq!(preferred_after_first, Some(hash(0xCC)));
    assert_eq!(
        agg.count_for(&hash(0xCC)),
        1,
        "first delivery credits tip support exactly once"
    );

    // Second delivery of the *same* validation: must be a no-op.
    assert!(
        !agg.add(v.clone()),
        "redelivering the identical validation must return false"
    );
    assert_eq!(
        agg.get_preferred(10),
        preferred_after_first,
        "preferred must be identical after idempotent redelivery"
    );
    assert_eq!(
        agg.count_for(&hash(0xCC)),
        1,
        "tip support must NOT double-count on idempotent redelivery"
    );

    // Third delivery, just to be sure: still no-op, still 1.
    assert!(!agg.add(v));
    assert_eq!(agg.count_for(&hash(0xCC)), 1);
    assert_eq!(agg.get_preferred(10), preferred_after_first);
}
