//! End-to-end integration test for the negative-UNL pseudo-transaction
//! pipeline (C-B8). Exercises the full cycle:
//!
//!   register_validators -> record_validation_into_engine over 256
//!   ledgers -> apply_negative_unl at flag ledger 256 -> NegativeUNL
//!   ledger entry written to state -> survives a subsequent
//!   ledger close.

use std::collections::{HashMap, HashSet};

use rxrpl_config::NodeConfig;
use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{
    ConsensusAdapter, ConsensusEngine, ConsensusParams, TrustedValidatorList,
};
use rxrpl_ledger::sle_codec;
use rxrpl_node::Node;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;
use serde_json::Value;

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

fn nid(id: u8) -> NodeId {
    NodeId(Hash256::new([id; 32]))
}

fn make_validation(id: u8, ledger_seq: u32) -> Validation {
    Validation {
        node_id: nid(id),
        public_key: Vec::new(),
        ledger_hash: Hash256::new([0xCC; 32]),
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

fn key_map(ids: &[u8]) -> HashMap<NodeId, String> {
    ids.iter()
        .map(|&i| (nid(i), format!("ED{:0>62}", hex::encode([i; 31]))))
        .collect()
}

#[test]
fn e2e_negative_unl_generates_and_applies_pseudo_txs() {
    // Real node (provides tx_engine, fees, ledger).
    let node = Node::new(NodeConfig::default()).unwrap();

    // Standalone consensus engine wired to the same trust set.
    let trusted: HashSet<NodeId> = (1u8..=5).map(nid).collect();
    let unl = TrustedValidatorList::new(trusted.clone());
    let mut consensus: ConsensusEngine<NoopAdapter> = ConsensusEngine::new_with_unl(
        NoopAdapter,
        nid(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );

    let keys = key_map(&[1, 2, 3, 4, 5]);
    consensus.register_validators(&trusted, &keys);

    // Simulate 256 ledgers. Validator 5 only validates one in four
    // ledgers (~25%) -- well below the reliability threshold.
    for i in 1..=256u32 {
        for j in 1..=5u8 {
            if j != 5 || i % 4 == 0 {
                Node::record_validation_into_engine(
                    &mut consensus,
                    &make_validation(j, i),
                );
            }
        }
        consensus.on_ledger_close_for_tracker();
    }

    // Apply negative-UNL pseudo-txs at flag ledger 256.
    let mut ledger = node.ledger().blocking_write();
    let results = Node::apply_negative_unl(
        &mut consensus,
        &mut ledger,
        node.tx_engine(),
        node.fees(),
        256,
    );

    assert!(!results.is_empty(), "flag ledger must produce at least one pseudo-tx");
    assert!(results[0].is_success(), "UNLModify pseudo-tx must apply successfully");

    // NegativeUNL ledger entry exists and lists validator 5.
    let nunl_data = ledger
        .get_state(&keylet::negative_unl())
        .expect("NegativeUNL SLE present after pseudo-tx");
    let nunl: Value = sle_codec::decode_state(nunl_data).unwrap();
    assert_eq!(nunl["LedgerEntryType"], "NegativeUNL");
    let disabled = nunl["DisabledValidators"].as_array().unwrap();
    assert_eq!(disabled.len(), 1, "validator 5 must be the sole demoted entry");
    let key5 = keys.get(&nid(5)).unwrap();
    assert_eq!(disabled[0]["PublicKey"].as_str().unwrap(), key5);

    // Engine UNL must be in sync with on-ledger state.
    assert!(consensus.unl().is_in_negative_unl(&nid(5)));

    // The entry must survive a subsequent ledger close (B7 invariant
    // exercised through the node's owned ledger).
    ledger.close(123, 0).unwrap();
    let after_close = ledger
        .get_state(&keylet::negative_unl())
        .expect("NegativeUNL must survive ledger close in the e2e flow");
    let reloaded: Value = sle_codec::decode_state(after_close).unwrap();
    assert_eq!(
        reloaded["DisabledValidators"]
            .as_array()
            .unwrap()
            .len(),
        1,
    );
}
