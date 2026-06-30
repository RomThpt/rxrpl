//! Integration tests for `Node::apply_negative_unl` — the node-level
//! pseudo-transaction generation entry point that mirrors
//! `apply_amendment_voting` for the negative UNL.
//!
//! Covers batches B4 and B5 of the nUNL pseudo-tx plan.

use std::collections::{HashMap, HashSet};

use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{ConsensusAdapter, ConsensusEngine, ConsensusParams, TrustedValidatorList};
use rxrpl_ledger::Ledger;
use rxrpl_node::Node;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::keylet;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use serde_json::Value;

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

fn node_id(id: u8) -> NodeId {
    NodeId(Hash256::new([id; 32]))
}

fn make_unl(ids: &[u8]) -> TrustedValidatorList {
    let trusted: HashSet<NodeId> = ids.iter().map(|&i| node_id(i)).collect();
    TrustedValidatorList::new(trusted)
}

fn make_key_map(ids: &[u8]) -> HashMap<NodeId, String> {
    ids.iter()
        .map(|&i| (node_id(i), format!("ED{:0>62}", hex::encode([i; 31]))))
        .collect()
}

fn make_engine(ids: &[u8]) -> ConsensusEngine<NoopAdapter> {
    ConsensusEngine::new_with_unl(
        NoopAdapter::new(),
        node_id(1),
        Vec::new(),
        ConsensusParams::default(),
        make_unl(ids),
    )
}

fn make_tx_engine_with_pseudo() -> TxEngine {
    let mut registry = TransactorRegistry::new();
    rxrpl_tx_engine::handlers::register_pseudo(&mut registry);
    TxEngine::new_without_sig_check(registry)
}

// ===== B4 =====

#[test]
fn apply_negative_unl_off_flag_ledger_is_noop() {
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    consensus.register_validators(&trusted, &key_map);

    let mut ledger = Ledger::genesis();
    let tx_engine = make_tx_engine_with_pseudo();
    let fees = FeeSettings::default();

    let results = Node::apply_negative_unl(&mut consensus, &mut ledger, &tx_engine, &fees, 100);
    assert!(
        results.is_empty(),
        "non-flag ledger must yield zero pseudo-txs"
    );
    assert!(ledger.get_state(&keylet::negative_unl()).is_none());
}

#[test]
fn apply_negative_unl_creates_unl_modify_tx_for_unreliable_validator() {
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    consensus.register_validators(&trusted, &key_map);

    // Validator 5 silent for the entire window.
    for _ in 0..256u32 {
        for id in 1..=4 {
            consensus.record_validation(node_id(id));
        }
        consensus.on_ledger_close_for_tracker();
    }

    let mut ledger = Ledger::genesis();
    let tx_engine = make_tx_engine_with_pseudo();
    let fees = FeeSettings::default();

    // Deferred move at flag 256 is a no-op (nothing pending yet).
    Node::update_negative_unl(&mut ledger, 256);

    let results = Node::apply_negative_unl(&mut consensus, &mut ledger, &tx_engine, &fees, 256);
    assert_eq!(results.len(), 1);
    assert!(results[0].is_success());

    let nunl_key = keylet::negative_unl();
    let key5 = key_map.get(&node_id(5)).unwrap();

    // Deferred model: the pseudo-tx only records ValidatorToDisable; the
    // DisabledValidators array is NOT touched yet.
    let data = ledger
        .get_state(&nunl_key)
        .expect("NegativeUNL SLE present after pseudo-tx");
    let obj: Value = rxrpl_ledger::sle_codec::decode_state(data).expect("decodes");
    assert_eq!(obj["ValidatorToDisable"].as_str().unwrap(), key5);
    assert!(
        obj.get("DisabledValidators").is_none(),
        "DisabledValidators must stay absent until the deferred move: {obj}"
    );

    // The deferred move at the NEXT flag ledger (512) moves the pending
    // validator into DisabledValidators with the wrapper + FirstLedgerSequence
    // stamp, and clears the pending field.
    Node::update_negative_unl(&mut ledger, 512);
    let data = ledger
        .get_state(&nunl_key)
        .expect("NegativeUNL still present");
    let obj: Value = rxrpl_ledger::sle_codec::decode_state(data).expect("decodes");
    let disabled = obj["DisabledValidators"].as_array().expect("array");
    assert_eq!(disabled.len(), 1);
    assert_eq!(
        disabled[0]["DisabledValidator"]["PublicKey"]
            .as_str()
            .unwrap(),
        key5
    );
    assert_eq!(
        disabled[0]["DisabledValidator"]["FirstLedgerSequence"]
            .as_u64()
            .unwrap(),
        512
    );
    assert!(obj.get("ValidatorToDisable").is_none());
}

// ===== B5 =====

#[test]
fn apply_negative_unl_re_enable_validator_on_next_flag_ledger() {
    // Window 1 (ledger 256): validator 5 silent -> disabled.
    // Window 2 (ledger 512): validator 5 fully reliable -> re-enabled.
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    let key_map = make_key_map(&[1, 2, 3, 4, 5]);
    consensus.register_validators(&trusted, &key_map);

    let mut ledger = Ledger::genesis();
    let tx_engine = make_tx_engine_with_pseudo();
    let fees = FeeSettings::default();

    let nunl_key = keylet::negative_unl();
    let key5 = key_map.get(&node_id(5)).unwrap().clone();

    // -- Flag ledger 256: validator 5 disabled (pending only). --
    for _ in 0..256u32 {
        for id in 1..=4 {
            consensus.record_validation(node_id(id));
        }
        consensus.on_ledger_close_for_tracker();
    }
    Node::update_negative_unl(&mut ledger, 256); // no-op, nothing pending
    let r1 = Node::apply_negative_unl(&mut consensus, &mut ledger, &tx_engine, &fees, 256);
    assert_eq!(r1.len(), 1);
    assert!(r1[0].is_success());

    // Pending ValidatorToDisable set; DisabledValidators still absent.
    let obj: Value =
        rxrpl_ledger::sle_codec::decode_state(ledger.get_state(&nunl_key).unwrap()).unwrap();
    assert_eq!(obj["ValidatorToDisable"].as_str().unwrap(), key5);
    assert!(obj.get("DisabledValidators").is_none());

    // -- Flag ledger 512: deferred move makes the disable effective, then
    //    validator 5 (now reliable) is re-enabled (pending only). --
    for _ in 0..256u32 {
        for id in 1..=5 {
            consensus.record_validation(node_id(id));
        }
        consensus.on_ledger_close_for_tracker();
    }
    Node::update_negative_unl(&mut ledger, 512);
    let obj: Value =
        rxrpl_ledger::sle_codec::decode_state(ledger.get_state(&nunl_key).unwrap()).unwrap();
    let disabled = obj["DisabledValidators"].as_array().unwrap();
    assert_eq!(disabled.len(), 1);
    assert_eq!(
        disabled[0]["DisabledValidator"]["PublicKey"]
            .as_str()
            .unwrap(),
        key5
    );
    assert_eq!(
        disabled[0]["DisabledValidator"]["FirstLedgerSequence"]
            .as_u64()
            .unwrap(),
        512
    );
    assert!(obj.get("ValidatorToDisable").is_none());

    let r2 = Node::apply_negative_unl(&mut consensus, &mut ledger, &tx_engine, &fees, 512);
    assert_eq!(r2.len(), 1);
    assert!(r2[0].is_success());

    // Re-enable is pending; the disabled entry is still on-ledger until moved.
    let obj: Value =
        rxrpl_ledger::sle_codec::decode_state(ledger.get_state(&nunl_key).unwrap()).unwrap();
    assert_eq!(obj["ValidatorToReEnable"].as_str().unwrap(), key5);
    assert_eq!(obj["DisabledValidators"].as_array().unwrap().len(), 1);
    // Engine UNL is synced as soon as the re-enable is decided.
    assert!(!consensus.unl().is_in_negative_unl(&node_id(5)));

    // -- Flag ledger 768: deferred move removes the last entry, erasing the
    //    NegativeUNL object (mirrors rippled rawErase). --
    Node::update_negative_unl(&mut ledger, 768);
    assert!(
        ledger.get_state(&nunl_key).is_none(),
        "NegativeUNL object must be erased once DisabledValidators is empty"
    );
}

#[test]
fn apply_negative_unl_no_changes_when_all_reliable() {
    let mut consensus = make_engine(&[1, 2, 3, 4, 5]);
    let trusted: HashSet<NodeId> = [1u8, 2, 3, 4, 5].iter().map(|&i| node_id(i)).collect();
    consensus.register_validators(&trusted, &make_key_map(&[1, 2, 3, 4, 5]));

    for _ in 0..256u32 {
        for id in 1..=5 {
            consensus.record_validation(node_id(id));
        }
        consensus.on_ledger_close_for_tracker();
    }

    let mut ledger = Ledger::genesis();
    let tx_engine = make_tx_engine_with_pseudo();
    let fees = FeeSettings::default();

    let results = Node::apply_negative_unl(&mut consensus, &mut ledger, &tx_engine, &fees, 256);
    assert!(results.is_empty());
    assert!(ledger.get_state(&keylet::negative_unl()).is_none());
}
