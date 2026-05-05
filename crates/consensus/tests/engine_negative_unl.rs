//! Integration tests for the negative-UNL pseudo-transaction generation
//! pipeline at the `ConsensusEngine` level.
//!
//! These tests cover the engine-side surface required for batches B1..B3
//! of the nUNL pseudo-tx plan:
//!   - B1: `register_validators` populates the tracker key map.
//!   - B2: `record_validation` accumulates validations in the tracker.
//!   - B3: `evaluate_negative_unl` produces UNLModify changes at flag
//!     ledgers and rotates the window.

use std::collections::{HashMap, HashSet};

use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{ConsensusAdapter, ConsensusEngine, ConsensusParams, TrustedValidatorList};
use rxrpl_primitives::Hash256;

struct NoopAdapter {
    accepted_ledger_hash: Hash256,
}

impl NoopAdapter {
    fn new() -> Self {
        Self {
            accepted_ledger_hash: Hash256::new([0xAA; 32]),
        }
    }
}

impl ConsensusAdapter for NoopAdapter {
    fn propose(&self, _: &Proposal) {}
    fn share_position(&self, _: &Proposal) {}
    fn share_tx(&self, _: &Hash256, _: &[u8]) {}
    fn acquire_tx_set(&self, _: &Hash256) -> Option<TxSet> {
        None
    }
    fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}
    fn on_accept(&self, _: &Validation) {}
    fn on_accept_ledger(&self, _: &TxSet, _: u32, _: u8) -> Hash256 {
        self.accepted_ledger_hash
    }
}

fn node(id: u8) -> NodeId {
    NodeId(Hash256::new([id; 32]))
}

fn make_unl(ids: &[u8]) -> TrustedValidatorList {
    let trusted: HashSet<NodeId> = ids.iter().map(|&i| node(i)).collect();
    TrustedValidatorList::new(trusted)
}

fn make_key_map(ids: &[u8]) -> HashMap<NodeId, String> {
    ids.iter()
        .map(|&i| (node(i), format!("ED{:0>62}", hex::encode([i; 31]))))
        .collect()
}

fn make_engine(ids: &[u8]) -> ConsensusEngine<NoopAdapter> {
    let unl = make_unl(ids);
    ConsensusEngine::new_with_unl(
        NoopAdapter::new(),
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    )
}

// ===== B1 =====

#[test]
fn engine_register_validators_populates_tracker_keys() {
    let mut engine = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);

    engine.register_validators(&trusted, &key_map);

    // Simulate window where validator 5 is silent: with keys registered,
    // a UNLModify(disable) change must be emitted for node 5.
    for _ in 0..256 {
        for id in 1..=4 {
            engine.record_validation(node(id));
        }
        engine.on_ledger_close_for_tracker();
    }

    let changes = engine.evaluate_negative_unl(256);
    let key5 = key_map.get(&node(5)).unwrap();
    assert_eq!(
        changes.len(),
        1,
        "expected one disable for absent validator 5"
    );
    assert!(changes[0].disable);
    assert_eq!(&changes[0].validator_key, key5);
}

// ===== B2 =====

#[test]
fn engine_records_validations_from_multiple_validators() {
    let mut engine = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    engine.register_validators(&trusted, &key_map);

    // All validators reliable: no changes.
    for _ in 0..256 {
        for id in 1..=5 {
            engine.record_validation(node(id));
        }
        engine.on_ledger_close_for_tracker();
    }

    let changes = engine.evaluate_negative_unl(256);
    assert!(
        changes.is_empty(),
        "no demotion when all validators are reliable"
    );
}

// ===== B3 =====

#[test]
fn engine_emits_unl_modify_changes_at_flag_ledger() {
    let mut engine = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    engine.register_validators(&trusted, &key_map);

    // Validator 5 only validates 100/256 (~39%) -> below threshold.
    for i in 0..256u32 {
        for id in 1..=4 {
            engine.record_validation(node(id));
        }
        if i < 100 {
            engine.record_validation(node(5));
        }
        engine.on_ledger_close_for_tracker();
    }

    // Non-flag ledger: no changes.
    let none = engine.evaluate_negative_unl(255);
    assert!(
        none.is_empty(),
        "evaluation off a flag ledger must yield no changes"
    );

    let changes = engine.evaluate_negative_unl(256);
    assert_eq!(changes.len(), 1);
    assert!(changes[0].disable);
    assert_eq!(changes[0].ledger_seq, 256);
    // Engine must sync UNL negative set with tracker.
    assert!(engine.unl().is_in_negative_unl(&node(5)));
}
