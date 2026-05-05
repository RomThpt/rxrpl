//! Integration test for batch C-B6: plumb overlay `Validation` messages
//! into the consensus engine's negative-UNL tracker via the
//! `Node::record_validation_into_engine` helper.

use std::collections::{HashMap, HashSet};

use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{ConsensusAdapter, ConsensusEngine, ConsensusParams, TrustedValidatorList};
use rxrpl_node::Node;
use rxrpl_primitives::Hash256;

struct NoopAdapter;

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
        Hash256::ZERO
    }
}

fn node_id(id: u8) -> NodeId {
    NodeId(Hash256::new([id; 32]))
}

fn make_validation(id: u8, ledger_seq: u32) -> Validation {
    Validation {
        node_id: node_id(id),
        public_key: Vec::new(),
        ledger_hash: Hash256::new([0xAA; 32]),
        ledger_seq,
        full: true,
        close_time: 100,
        sign_time: 100,
        signature: None,
        amendments: vec![],
        signing_payload: None,
        ..Default::default()
    }
}

fn make_engine(ids: &[u8]) -> ConsensusEngine<NoopAdapter> {
    let trusted: HashSet<NodeId> = ids.iter().map(|&i| node_id(i)).collect();
    ConsensusEngine::new_with_unl(
        NoopAdapter,
        node_id(1),
        Vec::new(),
        ConsensusParams::default(),
        TrustedValidatorList::new(trusted),
    )
}

fn make_key_map(ids: &[u8]) -> HashMap<NodeId, String> {
    ids.iter()
        .map(|&i| (node_id(i), format!("ED{:0>62}", hex::encode([i; 31]))))
        .collect()
}

#[test]
fn record_validation_into_engine_demotes_silent_validator() {
    // Drive the engine exclusively through `record_validation_into_engine`,
    // exactly as the consensus loop does on receipt of
    // `ConsensusMessage::Validation`. Validator 5 never produces a
    // validation, so it must be demoted at flag ledger 256.
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    consensus.register_validators(&trusted, &key_map);

    for seq in 1..=256u32 {
        for id in 1..=4u8 {
            let v = make_validation(id, seq);
            Node::record_validation_into_engine(&mut consensus, &v);
        }
        consensus.on_ledger_close_for_tracker();
    }

    let changes = consensus.evaluate_negative_unl(256);
    assert_eq!(
        changes.len(),
        1,
        "validator 5 must be demoted via overlay plumbing"
    );
    assert!(changes[0].disable);
    let key5 = key_map.get(&node_id(5)).unwrap();
    assert_eq!(&changes[0].validator_key, key5);
}

#[test]
fn record_validation_into_engine_keeps_reliable_validator() {
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    consensus.register_validators(&trusted, &make_key_map(&[1, 2, 3, 4, 5]));

    for seq in 1..=256u32 {
        for id in 1..=5u8 {
            let v = make_validation(id, seq);
            Node::record_validation_into_engine(&mut consensus, &v);
        }
        consensus.on_ledger_close_for_tracker();
    }

    let changes = consensus.evaluate_negative_unl(256);
    assert!(changes.is_empty(), "all validators reliable -> no changes");
}
