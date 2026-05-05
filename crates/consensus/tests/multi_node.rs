use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use rxrpl_consensus::types::{NodeId, Proposal, TxSet, Validation};
use rxrpl_consensus::{ConsensusAdapter, ConsensusEngine, ConsensusParams, TrustedValidatorList};
use rxrpl_primitives::Hash256;

struct MockAdapter {
    tx_sets: HashMap<Hash256, TxSet>,
    accepted_ledger_hash: Hash256,
    shared_positions: Mutex<Vec<Proposal>>,
}

impl MockAdapter {
    fn new() -> Self {
        Self {
            tx_sets: HashMap::new(),
            accepted_ledger_hash: Hash256::new([0xAA; 32]),
            shared_positions: Mutex::new(Vec::new()),
        }
    }

    fn with_tx_sets(sets: Vec<TxSet>) -> Self {
        let mut adapter = Self::new();
        for set in sets {
            adapter.tx_sets.insert(set.hash, set);
        }
        adapter
    }
}

impl ConsensusAdapter for MockAdapter {
    fn propose(&self, _: &Proposal) {}
    fn share_position(&self, p: &Proposal) {
        self.shared_positions.lock().unwrap().push(p.clone());
    }
    fn share_tx(&self, _: &Hash256, _: &[u8]) {}
    fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet> {
        self.tx_sets.get(hash).cloned()
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
    let mut trusted = HashSet::new();
    for id in ids {
        trusted.insert(node(*id));
    }
    TrustedValidatorList::new(trusted)
}

fn proposal_for(node_id: NodeId, tx_set_hash: Hash256, prev_ledger: Hash256, seq: u32) -> Proposal {
    Proposal {
        node_id,
        public_key: vec![0x02; 33],
        tx_set_hash,
        close_time: 100,
        prop_seq: 0,
        ledger_seq: seq,
        prev_ledger,
        signature: None,
    }
}

/// 3 engines with shared UNL, same tx set -> converge at round 1.
#[test]
fn three_nodes_same_set_converge() {
    let unl = make_unl(&[1, 2, 3]);
    let tx_set = TxSet::new(vec![Hash256::new([0x10; 32])]);

    // Engine for node 1
    let adapter1 = MockAdapter::with_tx_sets(vec![tx_set.clone()]);
    let mut engine1 = ConsensusEngine::new_with_unl(
        adapter1,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl.clone(),
    );

    // Engine for node 2
    let adapter2 = MockAdapter::with_tx_sets(vec![tx_set.clone()]);
    let mut engine2 = ConsensusEngine::new_with_unl(
        adapter2,
        node(2),
        Vec::new(),
        ConsensusParams::default(),
        unl.clone(),
    );

    // Engine for node 3
    let adapter3 = MockAdapter::with_tx_sets(vec![tx_set.clone()]);
    let mut engine3 = ConsensusEngine::new_with_unl(
        adapter3,
        node(3),
        Vec::new(),
        ConsensusParams::default(),
        unl.clone(),
    );

    let prev = Hash256::ZERO;
    let seq = 1;

    // All start round and close with same tx set
    engine1.start_round(prev, seq);
    engine1.close_ledger(tx_set.clone(), 100, seq).unwrap();

    engine2.start_round(prev, seq);
    engine2.close_ledger(tx_set.clone(), 100, seq).unwrap();

    engine3.start_round(prev, seq);
    engine3.close_ledger(tx_set.clone(), 100, seq).unwrap();

    // Exchange proposals: each engine receives the other two
    let p1 = engine1.our_position().unwrap().clone();
    let p2 = engine2.our_position().unwrap().clone();
    let p3 = engine3.our_position().unwrap().clone();

    // Anchor freshness against each proposal's own close_time so the
    // integration test's frozen-time model (close_time=100) does not
    // collide with the wall-clock check inside `peer_proposal`.
    engine1.peer_proposal_at(p2.clone(), p2.close_time);
    engine1.peer_proposal_at(p3.clone(), p3.close_time);

    engine2.peer_proposal_at(p1.clone(), p1.close_time);
    engine2.peer_proposal_at(p3.clone(), p3.close_time);

    engine3.peer_proposal_at(p1.clone(), p1.close_time);
    engine3.peer_proposal_at(p2.clone(), p2.close_time);

    // quorum = ceil(3*0.8) = 3, all 3 agree -> converge
    assert!(engine1.converge());
    assert!(engine2.converge());
    assert!(engine3.converge());
}

/// 2 vs 1 tx set disagreement -> dispute resolution + quorum convergence.
#[test]
fn two_vs_one_dispute_resolution() {
    let tx1 = Hash256::new([0x10; 32]);
    let tx2 = Hash256::new([0x20; 32]);

    let set_majority = TxSet::new(vec![tx1]);
    let set_minority = TxSet::new(vec![tx1, tx2]);

    // 5-node UNL, quorum = 4
    let unl = make_unl(&[1, 2, 3, 4, 5]);

    // Node 1 has the minority set
    let adapter1 = MockAdapter::with_tx_sets(vec![set_majority.clone(), set_minority.clone()]);
    let mut engine1 = ConsensusEngine::new_with_unl(
        adapter1,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl.clone(),
    );

    let prev = Hash256::ZERO;
    let seq = 1;

    engine1.start_round(prev, seq);
    engine1
        .close_ledger(set_minority.clone(), 100, seq)
        .unwrap();

    // Nodes 2, 3, 4, 5 all propose the majority set.
    // proposal_for sets close_time=100; anchor `now` accordingly.
    for id in [2, 3, 4, 5] {
        engine1.peer_proposal_at(proposal_for(node(id), set_majority.hash, prev, seq), 100);
    }

    // Converge: tx2 has only 1/5 support (us), should be dropped
    // After dispute resolution, set matches majority, quorum met
    let mut converged = false;
    for _ in 0..10 {
        if engine1.converge() {
            converged = true;
            break;
        }
    }
    assert!(converged, "engine should converge within max rounds");

    // Final set should be the majority set (only tx1)
    let final_set = engine1.our_set().unwrap();
    assert!(final_set.txs.contains(&tx1));
    assert!(!final_set.txs.contains(&tx2));
}

/// Solo mode (empty UNL) remains unchanged.
#[test]
fn solo_mode_empty_unl() {
    let adapter = MockAdapter::new();
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set, 100, 1).unwrap();

    assert!(engine.converge());
    assert!(engine.accepted_set().is_some());
}
