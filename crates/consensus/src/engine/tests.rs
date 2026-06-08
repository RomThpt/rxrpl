use super::*;
use std::sync::Mutex;

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

// Simple mock for backward compat
struct SimpleAdapter;
impl ConsensusAdapter for SimpleAdapter {
    fn propose(&self, _: &Proposal) {}
    fn share_position(&self, _: &Proposal) {}
    fn share_tx(&self, _: &Hash256, _: &[u8]) {}
    fn acquire_tx_set(&self, _: &Hash256) -> Option<TxSet> {
        None
    }
    fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}
    fn on_accept(&self, _: &Validation) {}
    fn on_accept_ledger(&self, _: &TxSet, _: u32, _: u8) -> Hash256 {
        Hash256::new([0xAA; 32])
    }
}

fn test_engine() -> ConsensusEngine<SimpleAdapter> {
    let node_id = NodeId(Hash256::new([0x01; 32]));
    ConsensusEngine::new(SimpleAdapter, node_id, ConsensusParams::default())
}

#[test]
fn initial_phase_is_open() {
    let engine = test_engine();
    assert_eq!(engine.phase(), ConsensusPhase::Open);
}

#[test]
fn close_ledger_transitions_to_establish() {
    let mut engine = test_engine();
    engine.start_round(Hash256::ZERO, 1);

    let tx_set = TxSet::new(vec![]);
    engine.close_ledger(tx_set, 100, 1).unwrap();
    assert_eq!(engine.phase(), ConsensusPhase::Establish);
}

#[test]
fn solo_consensus() {
    let mut engine = test_engine();
    engine.start_round(Hash256::ZERO, 1);

    let tx_set = TxSet::new(vec![]);
    engine.close_ledger(tx_set, 100, 1).unwrap();

    // With no peers, we have 100% agreement
    assert!(engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Accepted);
    assert!(engine.accepted_set().is_some());
}

#[test]
fn close_wrong_phase_fails() {
    let mut engine = test_engine();
    engine.start_round(Hash256::ZERO, 1);

    let tx_set = TxSet::new(vec![]);
    engine.close_ledger(tx_set.clone(), 100, 1).unwrap();

    // Already in establish, can't close again
    assert!(engine.close_ledger(tx_set, 200, 1).is_err());
}

// --- H1: Dispute resolution tests ---

#[test]
fn dispute_majority_drops_tx() {
    // 3 nodes: A={tx1,tx2}, B={tx1}, C={tx1}
    // A should drop tx2 since only 1/3 include it
    let tx1 = Hash256::new([0x01; 32]);
    let tx2 = Hash256::new([0x02; 32]);

    let set_a = TxSet::new(vec![tx1, tx2]);
    let set_bc = TxSet::new(vec![tx1]);

    let adapter = MockAdapter::with_tx_sets(vec![set_a.clone(), set_bc.clone()]);
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));
    let node_c = NodeId(Hash256::new([0xC0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(set_a, 100, 1).unwrap();

    // B and C propose set with only tx1
    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );
    engine.peer_proposal_at(
        Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );

    // Converge: threshold round 0 = 50%, tx2 has 1/3 = 33% -> drop
    assert!(engine.converge());

    // Our set should now match B/C's set (only tx1)
    let final_set = engine.our_set().unwrap();
    assert_eq!(final_set.txs.len(), 1);
    assert!(final_set.txs.contains(&tx1));
    assert!(!final_set.txs.contains(&tx2));
}

#[test]
fn dispute_increments_prop_seq() {
    let tx1 = Hash256::new([0x01; 32]);
    let tx2 = Hash256::new([0x02; 32]);

    let set_a = TxSet::new(vec![tx1, tx2]);
    let set_b = TxSet::new(vec![tx1]);

    let adapter = MockAdapter::with_tx_sets(vec![set_a.clone(), set_b.clone()]);
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));
    let node_c = NodeId(Hash256::new([0xC0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(set_a, 100, 1).unwrap();
    assert_eq!(engine.our_position().unwrap().prop_seq, 0);

    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );
    engine.peer_proposal_at(
        Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );

    engine.converge();
    // Position changed, so prop_seq should have incremented
    assert_eq!(engine.our_position().unwrap().prop_seq, 1);
}

#[test]
fn no_disputes_when_all_agree() {
    let tx1 = Hash256::new([0x01; 32]);
    let set = TxSet::new(vec![tx1]);

    let adapter = MockAdapter::with_tx_sets(vec![set.clone()]);
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );

    assert!(engine.disputes().is_empty());
    assert!(engine.converge());
}

#[test]
fn unknown_peer_set_no_crash() {
    // Peer proposes a set we can't acquire -> no crash, no disputes
    let tx1 = Hash256::new([0x01; 32]);
    let set = TxSet::new(vec![tx1]);

    let adapter = MockAdapter::new(); // no known sets
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(set, 100, 1).unwrap();

    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::new([0xFF; 32]), // unknown
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        100,
    );

    assert!(engine.disputes().is_empty());
}

// --- H2: Close time negotiation tests ---

#[test]
fn close_time_median_rounding() {
    let adapter = SimpleAdapter;
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));
    let node_c = NodeId(Hash256::new([0xC0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Use the proposal's own close_time as the freshness anchor so
    // each peer proposal lands with delta=0 against PROPOSAL_FRESHNESS_SECS.
    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        200,
    );
    engine.peer_proposal_at(
        Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 150,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        150,
    );

    assert!(engine.converge());
    // Vote-counting: {100→90, 150→150, 200→210} each have 1 vote — a
    // three-way split with no strict majority, i.e. no close-time
    // consensus. rippled's effCloseTime then closes at the "no opinion"
    // time (parentCloseTime + 1), computed identically by every node.
    // (rxrpl previously tie-broke to the latest bucket — a non-rippled
    // behavior that forked the ledger hash cross-impl.)
    let res = engine.adaptive_close_time().resolution();
    assert_eq!(
        engine.accepted_close_time(),
        Some(eff_close_time(0, res, 0))
    );
}

#[test]
fn round_close_time_function() {
    assert_eq!(round_close_time(145, 30), 150);
    assert_eq!(round_close_time(150, 30), 150);
    assert_eq!(round_close_time(130, 30), 120);
    assert_eq!(round_close_time(100, 0), 100);
}

#[test]
fn round_close_time_saturates_near_u32_max() {
    // prior to fix: (u32::MAX-30 + 60) wrapped, returning a tiny number
    let r = round_close_time(u32::MAX - 30, 30);
    assert!(r >= u32::MAX - 60, "expected near u32::MAX, got {}", r);
}

#[test]
fn eff_close_time_zero_passthrough() {
    // close_time == 0 is rippled's "untrusted close time" sentinel and
    // must propagate unchanged regardless of resolution or prior.
    assert_eq!(eff_close_time(0, 30, 0), 0);
    assert_eq!(eff_close_time(0, 30, 12345), 0);
    assert_eq!(eff_close_time(0, 0, 99), 0);
}

#[test]
fn eff_close_time_clamp_active() {
    // round_close_time(100, 30) == 90, but prior+1 == 121, so clamp wins.
    assert_eq!(eff_close_time(100, 30, 120), 121);
    // Rounded equals prior exactly => clamp to prior+1 (strictly greater).
    // round_close_time(150, 30) == 150, prior 150 -> must return 151.
    assert_eq!(eff_close_time(150, 30, 150), 151);
    // Rounded sits below prior by a wide margin.
    assert_eq!(eff_close_time(50, 10, 200), 201);
}

#[test]
fn eff_close_time_clamp_inactive() {
    // round_close_time(150, 30) == 150 > prior+1 == 101 -> rounded wins.
    assert_eq!(eff_close_time(150, 30, 100), 150);
    // round_close_time(145, 30) == 150 > prior+1 == 100 -> rounded wins.
    assert_eq!(eff_close_time(145, 30, 99), 150);
    // prior_close_time == 0 (genesis-like) and rounded > 1.
    assert_eq!(eff_close_time(60, 10, 0), 60);
}

#[test]
fn eff_close_time_resolution_zero() {
    // resolution == 0 makes round_close_time the identity, so eff_close_time
    // reduces to max(close_time, prior+1) for non-zero inputs.
    assert_eq!(eff_close_time(100, 0, 50), 100);
    assert_eq!(eff_close_time(100, 0, 100), 101);
    assert_eq!(eff_close_time(100, 0, 200), 201);
}

#[test]
fn close_time_spread_sets_flag() {
    let adapter = SimpleAdapter;
    let node_a = NodeId(Hash256::new([0xA0; 32]));
    let node_b = NodeId(Hash256::new([0xB0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Peer proposes time far away (spread > 30s resolution).
    // Anchor `now` to the peer's own close_time so the freshness
    // gate doesn't drop the message — this test exercises the
    // spread-flag path, not the freshness path.
    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        200,
    );

    engine.converge();
    assert_eq!(engine.accepted_close_flags(), 1);
}

// --- H5: Accept tests ---

#[test]
fn accept_creates_validation() {
    let mut engine = test_engine();
    engine.start_round(Hash256::ZERO, 1);

    let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
    engine.close_ledger(tx_set, 100, 1).unwrap();

    assert!(engine.converge());
    let val = engine.accepted_validation().unwrap();
    assert_eq!(val.ledger_hash, Hash256::new([0xAA; 32]));
    assert_eq!(val.ledger_seq, 1);
    assert!(val.full);
}

#[test]
fn accept_includes_negotiated_close_time() {
    let adapter = SimpleAdapter;
    let node_a = NodeId(Hash256::new([0xA0; 32]));

    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set, 150, 1).unwrap();

    assert!(engine.converge());
    // Solo: close_time = our time, rounded
    assert_eq!(engine.accepted_close_time(), Some(150));
    let val = engine.accepted_validation().unwrap();
    assert_eq!(val.close_time, 150);
}

// --- L1: UNL tests ---

use crate::unl::TrustedValidatorList;
use std::collections::HashSet;

/// Synthetic per-id "public key" for tests. 33 bytes (secp256k1
/// length), prefix `0x02` so the verifier path treats it as a
/// secp256k1-shaped key, with the id byte filling the rest. Distinct
/// per `id` so derived NodeIds don't collide.
fn test_pk(id: u8) -> Vec<u8> {
    let mut pk = vec![0x02; 33];
    for byte in pk.iter_mut().skip(1) {
        *byte = id;
    }
    pk
}

fn node(id: u8) -> NodeId {
    // Derive from the synthetic test public key so that validations
    // built with `validation_for(node(n), ...)` carry a matching
    // (node_id, public_key) pair for the C3 binding check in
    // `record_trusted_validation`.
    NodeId::from_public_key(&test_pk(id))
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

#[test]
fn untrusted_proposal_ignored_with_unl() {
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Node 99 is not trusted
    engine.peer_proposal_at(proposal_for(node(99), set.hash, Hash256::ZERO, 1), 100);
    assert!(engine.peer_positions.is_empty());
}

#[test]
fn trusted_proposal_accepted_with_unl() {
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Node 2 is trusted
    engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
    assert_eq!(engine.peer_positions.len(), 1);
}

#[test]
fn mismatched_prev_ledger_ignored() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Different prev_ledger
    let bad_prev = Hash256::new([0xFF; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, bad_prev, 1), 100);
    assert!(engine.peer_positions.is_empty());
}

#[test]
fn unl_quorum_not_met_does_not_accept() {
    // 5-node UNL, quorum = ceil(5*0.8) = 4
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Only nodes 2, 3 agree (+ us = 3 total, need 4)
    engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);

    assert!(!engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Establish);
}

#[test]
fn unl_quorum_met_accepts() {
    // 5-node UNL, quorum = 4. Us (node 1) + nodes 2, 3, 4 = 4.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, Hash256::ZERO, 1), 100);

    assert!(engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Accepted);
}

#[test]
fn min_consensus_time_floor_holds_round_open() {
    // With a non-zero min_consensus_time_ms, converge() must NOT accept
    // even when quorum agrees, until the floor has elapsed. Mirrors
    // rippled's ledgerMIN_CONSENSUS gate.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let params = ConsensusParams {
        min_consensus_time_ms: 80,
        ..ConsensusParams::default()
    };
    let mut engine = ConsensusEngine::new_with_unl(SimpleAdapter, node(1), Vec::new(), params, unl);
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();
    engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, Hash256::ZERO, 1), 100);

    // Quorum is met, but the floor has not elapsed → round stays open.
    assert!(!engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Establish);

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Floor elapsed → same quorum now accepts.
    assert!(engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Accepted);
}

#[test]
fn solo_mode_unchanged_with_empty_unl() {
    // Empty UNL = solo mode, should still converge immediately
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set, 100, 1).unwrap();

    assert!(engine.converge());
    assert_eq!(engine.phase(), ConsensusPhase::Accepted);
}

// --- Future-proposal holding pen tests ---

#[test]
fn future_proposal_held_when_prev_ledger_unknown() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let future_prev = Hash256::new([0xAA; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev, 2), 100);

    // Held, not accepted.
    assert!(engine.peer_positions.is_empty());
    assert_eq!(
        engine.future_proposals.get(&future_prev).map(|v| v.len()),
        Some(1)
    );
}

#[test]
fn proposals_held_pending_prev_ledger_counter_bumps_on_hold() {
    // T34: every time peer_proposal_at routes a proposal into the
    // future_proposals holding pen because prev_ledger != self.prev_ledger,
    // the proposals_held_pending_prev_ledger_total counter must increment.
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();
    assert_eq!(engine.proposals_held_pending_prev_ledger_total(), 0);

    let future_prev_a = Hash256::new([0xA1; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev_a, 2), 100);
    assert_eq!(engine.proposals_held_pending_prev_ledger_total(), 1);

    // A different peer holding on the same future prev_ledger still
    // bumps the counter (each held proposal counts).
    engine.peer_proposal_at(proposal_for(node(3), set.hash, future_prev_a, 2), 100);
    assert_eq!(engine.proposals_held_pending_prev_ledger_total(), 2);

    // A peer on yet another future prev_ledger also counts.
    let future_prev_b = Hash256::new([0xB2; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev_b, 2), 100);
    assert_eq!(engine.proposals_held_pending_prev_ledger_total(), 3);

    // A proposal whose prev_ledger MATCHES our current one is NOT held
    // and must NOT bump the counter.
    engine.peer_proposal_at(proposal_for(node(2), set.hash, Hash256::ZERO, 1), 100);
    assert_eq!(engine.proposals_held_pending_prev_ledger_total(), 3);
}

#[test]
fn held_proposal_replayed_on_matching_start_round() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let future_prev = Hash256::new([0xBB; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, future_prev, 2), 100);
    assert!(engine.future_proposals.contains_key(&future_prev));

    // Catch up to the future ledger.
    engine.start_round(future_prev, 2);
    // Held proposal is moved to pending, ready for replay in close_ledger.
    assert!(!engine.future_proposals.contains_key(&future_prev));
    assert_eq!(engine.pending_proposals.len(), 1);

    // Replay path: close_ledger drains pending_proposals.
    let new_set = TxSet::new(vec![]);
    engine.close_ledger(new_set.clone(), 100, 2).unwrap();
    assert_eq!(engine.peer_positions.len(), 1);
    assert!(engine.peer_positions.contains_key(&node(2)));
}

#[test]
fn held_proposal_evicted_when_too_stale() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Hold a stale proposal (seq=2)
    let stale_prev = Hash256::new([0xCC; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, stale_prev, 2), 100);
    assert!(engine.future_proposals.contains_key(&stale_prev));

    // Jump forward many rounds. Stale proposals get evicted.
    engine.start_round(
        Hash256::new([0xDD; 32]),
        2 + FUTURE_PROPOSALS_STALE_LEDGERS + 1,
    );
    assert!(engine.future_proposals.is_empty());
}

#[test]
fn hold_dedups_per_node() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let prev = Hash256::new([0xEE; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, prev, 2), 100);
    // Same node, same key: only the latest is kept.
    assert_eq!(engine.future_proposals.get(&prev).map(|v| v.len()), Some(1));
}

#[test]
fn hold_caps_global_keys() {
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    // Push proposals with FUTURE_PROPOSALS_MAX_KEYS + 5 distinct prev_ledger
    // keys. The map should never grow past the cap.
    for i in 0u8..(FUTURE_PROPOSALS_MAX_KEYS as u8 + 5) {
        let mut h = [0u8; 32];
        h[0] = i.wrapping_add(1);
        let p = proposal_for(node(2), set.hash, Hash256::new(h), 100 + i as u32);
        // proposal_for produces close_time=100; anchor `now` accordingly.
        engine.peer_proposal_at(p, 100);
    }
    assert!(engine.future_proposals.len() <= FUTURE_PROPOSALS_MAX_KEYS);
}

// --- Wrong prev_ledger recovery tests ---

#[test]
fn wrong_prev_ledger_not_detected_in_solo_mode() {
    // Solo mode (empty UNL) should never trigger wrong prev_ledger.
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let bad_prev = Hash256::new([0xFF; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, bad_prev, 1), 100);

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn wrong_prev_ledger_detected_with_supermajority() {
    // 5-node UNL. 4 peers send proposals with a different prev_ledger.
    // 4/4 trusted disagree = 100% > 60% threshold.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    let our_prev = Hash256::ZERO;
    engine.start_round(our_prev, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let peer_prev = Hash256::new([0xBB; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(5), set.hash, peer_prev, 1), 100);

    let result = engine.check_wrong_prev_ledger();
    assert!(result.is_some());
    let detected = result.unwrap();
    assert_eq!(detected.preferred_ledger, peer_prev);
    assert_eq!(detected.peer_count, 4);
    assert_eq!(detected.total_trusted, 4); // 0 agreeing + 4 disagreeing
}

#[test]
fn wrong_prev_ledger_not_detected_below_threshold() {
    // 5-node UNL. 1 peer disagrees out of 4 who sent proposals.
    // 1/4 = 25% < 60%, should NOT trigger.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let peer_prev = Hash256::new([0xBB; 32]);
    // 1 peer disagrees
    engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
    // 3 peers agree
    engine.peer_proposal_at(proposal_for(node(3), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, Hash256::ZERO, 1), 100);
    engine.peer_proposal_at(proposal_for(node(5), set.hash, Hash256::ZERO, 1), 100);

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn wrong_prev_ledger_at_exact_threshold() {
    // 5-node UNL. 3 peers disagree, 2 agree.
    // 3/5 = 60% >= 60%, should trigger.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let peer_prev = Hash256::new([0xBB; 32]);
    // 3 peers disagree
    engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, peer_prev, 1), 100);
    // 2 peers agree
    engine.peer_proposal_at(proposal_for(node(5), set.hash, Hash256::ZERO, 1), 100);

    // total_trusted = 1 agreeing + 3 disagreeing = 4 (node(5) is agreeing, nodes 2,3,4 disagree)
    // pct = 3/4 = 75% >= 60%
    let result = engine.check_wrong_prev_ledger();
    assert!(result.is_some());
    let detected = result.unwrap();
    assert_eq!(detected.preferred_ledger, peer_prev);
    assert_eq!(detected.peer_count, 3);
}

#[test]
fn wrong_prev_ledger_untrusted_not_counted() {
    // 3-node UNL (1,2,3). Untrusted nodes (50,51) disagree.
    // Only trusted count, so 0 trusted disagree -> no detection.
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let peer_prev = Hash256::new([0xBB; 32]);
    // Untrusted nodes disagree
    engine.peer_proposal_at(proposal_for(node(50), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(51), set.hash, peer_prev, 1), 100);

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn wrong_prev_ledger_cleared_on_new_round() {
    // After start_round, wrong_prev_ledger tracking resets.
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let peer_prev = Hash256::new([0xBB; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, peer_prev, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, peer_prev, 1), 100);

    // Should detect (2/2 = 100%)
    assert!(engine.check_wrong_prev_ledger().is_some());

    // Start new round, tracking should be cleared.
    engine.start_round(peer_prev, 2);
    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn wrong_prev_ledger_picks_most_popular() {
    // 5-node UNL. 2 peers reference ledger A, 2 reference ledger B.
    // Each is 2/4 = 50% < 60%, so no detection.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();

    let prev_a = Hash256::new([0xAA; 32]);
    let prev_b = Hash256::new([0xBB; 32]);
    engine.peer_proposal_at(proposal_for(node(2), set.hash, prev_a, 1), 100);
    engine.peer_proposal_at(proposal_for(node(3), set.hash, prev_a, 1), 100);
    engine.peer_proposal_at(proposal_for(node(4), set.hash, prev_b, 1), 100);
    engine.peer_proposal_at(proposal_for(node(5), set.hash, prev_b, 1), 100);

    // 2/4 = 50% < 60% -> no detection
    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

// --- T17: ValidationsTrie wiring into wrong-prev-ledger detection ---

fn validation_for(node_id: NodeId, ledger_hash: Hash256, ledger_seq: u32) -> Validation {
    // Recover the synthetic test public key matching `node_id`. We
    // tried each `id` byte 0..=255 because the test helper `node(id)`
    // derives via `from_public_key(test_pk(id))`. Falling back to an
    // empty key keeps unrelated callers compiling but will fail the
    // C3 binding check (intentional — test_node_id_mismatch_rejected
    // exercises that path).
    let public_key = (0u8..=255)
        .find(|id| node(*id) == node_id)
        .map(test_pk)
        .unwrap_or_default();
    Validation {
        node_id,
        public_key,
        ledger_hash,
        ledger_seq,
        full: true,
        close_time: 100,
        sign_time: 100,
        ..Default::default()
    }
}

#[test]
fn check_wrong_prev_ledger_none_when_trie_and_proposals_empty() {
    // 5-node UNL, no proposals, no validations recorded -> None.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn check_wrong_prev_ledger_via_validations_trie_supermajority() {
    // 5-node UNL. 4 trusted validators record validations for a hash
    // that is NOT our prev_ledger. 4/5 = 80% >= 60% threshold.
    // Detection must come from the validations trie even though no
    // peer proposals have been received this round.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let preferred = Hash256::new([0xCC; 32]);
    for n in 2u8..=5 {
        assert!(engine.record_trusted_validation(validation_for(node(n), preferred, 0)));
    }

    let detected = engine.check_wrong_prev_ledger().expect("must trigger");
    assert_eq!(detected.preferred_ledger, preferred);
    assert_eq!(detected.peer_count, 4);
    assert_eq!(detected.total_trusted, 5);
}

#[test]
fn check_wrong_prev_ledger_validations_trie_agrees_with_us_returns_none() {
    // 3-node UNL. All trusted validators validated OUR prev_ledger.
    // No peer proposals. The trie's preferred branch == prev_ledger,
    // so neither stage fires.
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    let our_prev = Hash256::new([0xAA; 32]);
    engine.start_round(our_prev, 1);

    for n in 2u8..=3 {
        engine.record_trusted_validation(validation_for(node(n), our_prev, 0));
    }

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn check_wrong_prev_ledger_validations_trie_below_threshold_falls_through() {
    // 5-node UNL. Only 1 trusted validation for an alternative
    // (1/5 = 20% < 60%). With no proposal-derived disagreement
    // either, detection must return None — the trie path doesn't
    // promote a minority hash.
    let unl = make_unl(&[1, 2, 3, 4, 5]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);

    let alt = Hash256::new([0xDD; 32]);
    engine.record_trusted_validation(validation_for(node(2), alt, 0));

    assert_eq!(engine.check_wrong_prev_ledger(), None);
}

#[test]
fn record_trusted_validation_ignores_untrusted_node() {
    // Node 99 is not in the UNL. record_trusted_validation must
    // return false and the trie must remain empty.
    let unl = make_unl(&[1, 2, 3]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );

    let alt = Hash256::new([0xEE; 32]);
    assert!(!engine.record_trusted_validation(validation_for(node(99), alt, 0)));
    assert_eq!(engine.validations_trie().count_for(&alt), 0);
}

#[test]
fn record_trusted_validation_rejects_node_id_public_key_mismatch() {
    // Audit pass 2 C3: a validation whose node_id does NOT derive
    // from its public_key must be rejected, even when the node is
    // trusted — otherwise a forged validation can be attributed to
    // any UNL member by lying about the node_id field.
    let unl = make_unl(&[1, 2]);
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.add_trusted_validator(node(2));

    let alt = Hash256::new([0x55; 32]);
    // Build a validation that *claims* to be from node(2) but
    // carries a public_key for node(3). The binding check must
    // reject it before it touches the trie.
    let mut forged = validation_for(node(2), alt, 0);
    forged.public_key = test_pk(3);

    assert!(
        !engine.record_trusted_validation(forged),
        "node_id/public_key mismatch must be rejected"
    );
    assert_eq!(
        engine.validations_trie().count_for(&alt),
        0,
        "forged vote must not enter the trie"
    );

    // A validation with an empty public_key is also rejected
    // (the empty-key digest does not match any node(n)).
    let mut empty_key = validation_for(node(2), alt, 0);
    empty_key.public_key = vec![];
    assert!(
        !engine.record_trusted_validation(empty_key),
        "empty public_key must be rejected"
    );
    assert_eq!(engine.validations_trie().count_for(&alt), 0);
}

#[test]
fn add_then_remove_trusted_validator_flows_through_trie() {
    // Solo-mode engine (empty UNL). Enrol node 7 via the new
    // add_trusted_validator helper, record a validation, then remove
    // the node — its contribution must be decremented out.
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

    let alt = Hash256::new([0x77; 32]);
    // Pre-enrolment: validation is dropped.
    assert!(!engine.record_trusted_validation(validation_for(node(7), alt, 0)));

    engine.add_trusted_validator(node(7));
    assert!(engine.record_trusted_validation(validation_for(node(7), alt, 0)));
    assert_eq!(engine.validations_trie().count_for(&alt), 1);

    engine.remove_trusted_validator(&node(7));
    assert_eq!(engine.validations_trie().count_for(&alt), 0);
}

// --- Adaptive close-time resolution tests ---

#[test]
fn adaptive_resolution_starts_at_params_value() {
    let engine = test_engine();
    assert_eq!(engine.close_time_resolution(), 30);
}

#[test]
fn solo_rounds_count_as_agreement() {
    // Solo mode (no peers) treats every round as agreement, so the
    // rippled `getNextLedgerTimeResolution` cadence steps one bin
    // finer at every multiple of `INCREASE_LEDGER_TIME_RESOLUTION_EVERY`
    // (= 8).  Starting at the default 30s bin (index 2), the first
    // tightening fires when `start_round` is called with seq == 8.
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

    for seq in 1..=8 {
        engine.start_round(Hash256::ZERO, seq);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 100, seq).unwrap();
        engine.converge();
    }

    // 8 agreed rounds: at seq == 8 the modulo gate fires and the
    // bin steps from 30s (index 2) to 20s (index 1) — one bin
    // finer, matching rippled.
    assert_eq!(engine.close_time_resolution(), 20);
}

#[test]
fn close_time_agreement_tightens_resolution() {
    // Run 8 consecutive rounds where the peer agrees on the
    // close-time bucket (spread of 0 ≤ resolution).  The first 7
    // are no-ops at the bin level — only the call to `start_round`
    // for seq == 8 (multiple of the rippled
    // `INCREASE_LEDGER_TIME_RESOLUTION_EVERY = 8` cadence) triggers
    // a step finer.  Starting at 30s (index 2) the bin moves to
    // 20s (index 1).
    let adapter = SimpleAdapter;
    let node_a = node(0xA0);
    let node_b = node(0xB0);
    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

    for seq in 1..=8u32 {
        engine.start_round(Hash256::ZERO, seq);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, seq).unwrap();

        // Peer proposes same close time
        engine.peer_proposal_at(
            Proposal {
                node_id: node_b,
                public_key: vec![0x02; 33],
                tx_set_hash: set.hash,
                close_time: 100,
                prop_seq: 0,
                ledger_seq: seq,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
            100,
        );

        engine.converge();
    }

    // 8 agreed rounds -> bin stepped one finer: 30s -> 20s.
    assert_eq!(engine.close_time_resolution(), 20);
}

#[test]
fn close_time_disagreement_loosens_resolution() {
    // The rippled `getNextLedgerTimeResolution` cadence widens the
    // bin on disagreement at every multiple of
    // `DECREASE_LEDGER_TIME_RESOLUTION_EVERY = 1`, i.e. on the very
    // next `start_round` after the disagreement is observed.
    // Default starts at 30s; one disagreed round followed by a
    // fresh start_round steps the bin one COARSER to 60s.
    let adapter = SimpleAdapter;
    let node_a = node(0xA0);
    let node_b = node(0xB0);
    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

    // Round 1: peers disagree (spread of 100 > 30s resolution).
    // `accept` records `previous_close_agreed = false`.
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 1).unwrap();
    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200, // spread of 100 > 30
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        200,
    );
    engine.converge();
    // The disagreement flag is set, but the bin step happens on
    // the NEXT `start_round` (rippled recomputes resolution at the
    // top of each round, not in `accept`).
    assert_eq!(engine.accepted_close_flags(), 1);
    assert_eq!(engine.close_time_resolution(), 30);

    // Round 2: `start_round` calls
    // `next_resolution(30, false, 2)` → 60 (one bin coarser).
    engine.start_round(Hash256::ZERO, 2);
    assert_eq!(engine.close_time_resolution(), 60);
}

#[test]
fn adaptive_resolution_used_for_rounding() {
    // Verify that once the bin steps finer through the rippled
    // cadence, downstream close-time rounding uses the NEW bin
    // (not the params-default).  Run 8 agreed solo rounds to
    // step from 30s -> 20s, then submit a round whose proposals
    // straddle a 20s boundary and check the accepted close time.
    let adapter = SimpleAdapter;
    let node_a = node(0xA0);
    let node_b = node(0xB0);
    let mut engine = ConsensusEngine::new(adapter, node_a, ConsensusParams::default());

    for seq in 1..=8 {
        engine.start_round(Hash256::ZERO, seq);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 100, seq).unwrap();
        engine.converge();
    }
    assert_eq!(engine.close_time_resolution(), 20);

    // Round 9: peer proposes a close_time within the 20s bin so
    // the round still counts as agreement.  Both 100 and 105 round
    // to bucket 100 at resolution 20 ((100+10)/20*20 = 100,
    // (105+10)/20*20 = 100), giving an unambiguous winner.
    engine.start_round(Hash256::ZERO, 9);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set.clone(), 100, 9).unwrap();
    engine.peer_proposal_at(
        Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 105,
            prop_seq: 0,
            ledger_seq: 9,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
        105,
    );
    engine.converge();

    // Both proposals round to bucket 100 at resolution 20 → winner is 100.
    assert_eq!(engine.accepted_close_time(), Some(100));
}

#[test]
fn adaptive_resolution_getter_reflects_state() {
    // The engine drives `AdaptiveCloseTime` exclusively through
    // `set_resolution` now (T03), so the legacy
    // `consecutive_agreements` counter stays at 0.  The visible
    // state to assert is `resolution()`: stable at the default 30s
    // bin until the modulo cadence fires at seq == 8.
    let adapter = SimpleAdapter;
    let mut engine = ConsensusEngine::new(adapter, node(1), ConsensusParams::default());

    assert_eq!(engine.adaptive_close_time().resolution(), 30);

    // Seq 1: agreement (solo) but seq % 8 != 0 → resolution holds.
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set, 100, 1).unwrap();
    engine.converge();
    assert_eq!(engine.adaptive_close_time().resolution(), 30);

    // Walk to seq == 8 — the cadence fires and the bin tightens.
    for seq in 2..=8 {
        engine.start_round(Hash256::ZERO, seq);
        let set = TxSet::new(vec![]);
        engine.close_ledger(set, 100, seq).unwrap();
        engine.converge();
    }
    assert_eq!(engine.adaptive_close_time().resolution(), 20);
}

#[test]
fn monotonic_close_time_across_rounds() {
    // Verifies that `eff_close_time` is wired into the establish-phase
    // aggregation: when a round closes with a `close_time` that would
    // round to <= `prior_close_time`, the engine's accepted close time
    // is clamped to `prior_close_time + 1` (rippled `effCloseTime`
    // monotonicity guarantee).
    let mut engine = test_engine();

    // Round 1: parent close_time = 100. Our raw close_time = 50 would
    // round to 60 at the default 30s resolution, but the clamp pushes
    // it to 101.
    engine.start_round_with_prior(Hash256::ZERO, 1, 100);
    engine.close_ledger(TxSet::new(vec![]), 50, 1).unwrap();
    assert!(engine.converge());
    let ct1 = engine.accepted_close_time().expect("round 1 close time");
    assert!(
        ct1 >= 101,
        "round 1 close_time {} should be >= prior+1 (101)",
        ct1
    );

    // Round 2: parent close_time = ct1 (>= 101). Same raw close_time
    // 50, must clamp to >= ct1 + 1 (>= 102), demonstrating monotonicity
    // across rounds.
    engine.start_round_with_prior(Hash256::ZERO, 2, ct1);
    engine.close_ledger(TxSet::new(vec![]), 50, 2).unwrap();
    assert!(engine.converge());
    let ct2 = engine.accepted_close_time().expect("round 2 close time");
    assert!(
        ct2 > ct1,
        "round 2 close_time {} should be strictly greater than round 1 ({})",
        ct2,
        ct1
    );
}

// --- H11: peer close_time votes are FILTERED, not clamped ---

/// Two adversarial peers send close_time = 1 and close_time = 2,
/// way below `prior_close_time + 1 = 1_000_001`. The pre-fix code
/// applied `eff_close_time` to each peer vote, which clamped both
/// to `prior + 1 = 1_000_001` and counted them as TWO votes for
/// the same floor bucket — manufactured majority, forcing our own
/// fresh close_time bucket to lose. The fix FILTERS those votes
/// out before bucketing, so our honest fresh vote wins.
#[test]
fn h11_adversarial_low_close_times_dont_manufacture_agreement() {
    let prior = 1_000_000u32;
    let our_time = prior + 5; // fresh, well above the floor
    let mut engine = test_engine();
    engine.start_round_with_prior(Hash256::ZERO, 1, prior);
    engine
        .close_ledger(TxSet::new(vec![]), our_time, 1)
        .unwrap();

    // Manually inject two adversarial peer positions with garbage
    // close_times (below `prior + 1`). Bypassing peer_proposal_at
    // sidesteps the freshness gate, which would otherwise reject
    // these proposals before they ever reach the bucket logic — the
    // freshness gate is a separate defense; H11 is specifically
    // about what happens when garbage *does* reach the bucket logic
    // (e.g. from a future code path that bypasses freshness, or from
    // adversarial peers exploiting clock skew).
    let our_set_hash = engine.our_set().unwrap().hash;
    engine.peer_positions.insert(
        node(2),
        Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: 1,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
    );
    engine.peer_positions.insert(
        node(3),
        Proposal {
            node_id: node(3),
            public_key: vec![0x02; 33],
            tx_set_hash: our_set_hash,
            close_time: 2,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        },
    );

    let (ct, _) = engine.effective_close_time();
    // The accepted close_time MUST come from our honest fresh vote
    // (rounded to the 30s bucket), then clamped to >= prior + 1.
    // It MUST NOT collapse to the floor bucket (prior + 1) by way
    // of two adversarial votes that pre-fix were silently rewritten.
    let our_rounded = round_close_time(our_time, 30);
    let expected = eff_close_time(our_rounded, 30, prior);
    assert_eq!(
        ct, expected,
        "expected our honest bucket {} (eff = {}), got {} — adversarial peers manufactured agreement",
        our_rounded, expected, ct
    );
    // Belt-and-braces: the floor bucket prior+1 = 1_000_001 should
    // NOT win, since no honest voter put their vote there.
    assert_ne!(
        ct,
        prior + 1,
        "close_time collapsed to the monotonicity floor — bucket clamping resurfaced"
    );
}

/// `align_close_time_with_peers` must apply the same filter so
/// adversarial low close_times can't force a strict-majority
/// realignment of our position.
#[test]
fn h11_align_does_not_realign_to_floor_from_adversarial_votes() {
    let prior = 1_000_000u32;
    let our_time = prior + 35; // would round to a different bucket than floor
    let mut engine = test_engine();
    engine.start_round_with_prior(Hash256::ZERO, 1, prior);
    engine
        .close_ledger(TxSet::new(vec![]), our_time, 1)
        .unwrap();
    let our_set_hash = engine.our_set().unwrap().hash;

    // Three adversarial peers all sending garbage close_times.
    let adversarial: [(u8, u32); 3] = [(2, 0), (3, 1), (4, 2)];
    for (i, ct) in adversarial {
        engine.peer_positions.insert(
            node(i),
            Proposal {
                node_id: node(i),
                public_key: vec![0x02; 33],
                tx_set_hash: our_set_hash,
                close_time: ct,
                prop_seq: 0,
                ledger_seq: 1,
                prev_ledger: Hash256::ZERO,
                signature: None,
            },
        );
    }

    let our_pos_before = engine.our_position.as_ref().unwrap().close_time;
    engine.align_close_time_with_peers();
    let our_pos_after = engine.our_position.as_ref().unwrap().close_time;
    // No realignment must have occurred: adversarial votes were
    // filtered out, leaving only our own bucket — which doesn't
    // have a strict majority of the (us + peers) cohort.
    assert_eq!(
        our_pos_before, our_pos_after,
        "align_close_time_with_peers realigned us to a manufactured majority bucket"
    );
}

// --- T14: proposal freshness (propRELAY_INTERVAL) tests ---

fn freshness_engine() -> ConsensusEngine<SimpleAdapter> {
    // 2-node UNL with us=node(1), peer=node(2). Drives the engine into
    // Establish phase via close_ledger so peer_proposal_at exercises the
    // freshness gate (it short-circuits in non-Establish phases).
    let unl = make_unl(&[1, 2]);
    let mut engine = ConsensusEngine::new_with_unl(
        SimpleAdapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    let set = TxSet::new(vec![]);
    engine.close_ledger(set, 1_000_000, 1).unwrap();
    engine
}

#[test]
fn fresh_proposal_accepted() {
    let mut engine = freshness_engine();
    let now = 1_000_000u32;
    let our_set_hash = engine.our_set().unwrap().hash;
    let p = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: our_set_hash,
        close_time: now, // delta = 0 < PROPOSAL_FRESHNESS_SECS
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(p, now);
    assert_eq!(engine.proposal_dropped_stale_total(), 0);
    assert_eq!(engine.peer_positions.len(), 1);
}

#[test]
fn stale_proposal_rejected_increments_counter() {
    let mut engine = freshness_engine();
    let now = 1_000_000u32;
    let our_set_hash = engine.our_set().unwrap().hash;
    let stale = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: our_set_hash,
        close_time: now - 100, // delta = 100 > 30
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(stale, now);
    assert_eq!(engine.proposal_dropped_stale_total(), 1);
    assert!(engine.peer_positions.is_empty());
}

#[test]
fn future_proposal_rejected_increments_counter() {
    let mut engine = freshness_engine();
    let now = 1_000_000u32;
    let our_set_hash = engine.our_set().unwrap().hash;

    // First feed a stale proposal so we can verify the counter
    // accumulates across calls (== 2 after this test, per spec).
    let stale = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: our_set_hash,
        close_time: now - 100,
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(stale, now);
    assert_eq!(engine.proposal_dropped_stale_total(), 1);

    let future = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: our_set_hash,
        close_time: now + 100, // delta = 100 > 30
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(future, now);
    assert_eq!(engine.proposal_dropped_stale_total(), 2);
    assert!(engine.peer_positions.is_empty());
}

// --- Audit pass 2 C2: pending_proposals gating in Open phase ---

/// Engine in Open phase (no `close_ledger` so phase != Establish), with
/// a 2-node UNL trusting nodes 1 and 2. Used to exercise the
/// pre-Establish UNL/freshness/cap gates on `pending_proposals`.
fn open_phase_engine() -> ConsensusEngine<SimpleAdapter> {
    let unl = make_unl(&[1, 2]);
    let mut engine = ConsensusEngine::new_with_unl(
        SimpleAdapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    debug_assert_eq!(engine.phase(), ConsensusPhase::Open);
    engine
}

#[test]
fn pending_proposals_drops_untrusted_in_open_phase() {
    // Audit pass 2 C2: an untrusted peer must not be able to push into
    // pending_proposals during Open phase. Without the gate this is the
    // primary memory-amplification vector.
    let mut engine = open_phase_engine();
    let now = 1_000_000u32;
    let p = Proposal {
        node_id: node(99), // not in UNL
        public_key: vec![0x02; 33],
        tx_set_hash: Hash256::ZERO,
        close_time: now,
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(p, now);
    assert!(engine.pending_proposals.is_empty());
}

#[test]
fn pending_proposals_drops_stale_in_open_phase_and_bumps_counter() {
    // Audit pass 2 C2: a trusted but stale-time proposal must be
    // counted and dropped, never buffered. Mirrors the Establish-phase
    // freshness gate.
    let mut engine = open_phase_engine();
    let now = 1_000_000u32;
    let stale = Proposal {
        node_id: node(2), // trusted
        public_key: vec![0x02; 33],
        tx_set_hash: Hash256::ZERO,
        close_time: now - 100, // delta = 100 > PROPOSAL_FRESHNESS_SECS
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(stale, now);
    assert_eq!(engine.proposal_dropped_stale_total(), 1);
    assert!(engine.pending_proposals.is_empty());
}

#[test]
fn pending_proposals_capped_in_open_phase() {
    // Audit pass 2 C2: even when every proposal passes UNL + freshness,
    // pending_proposals must be capped at PENDING_PROPOSALS_MAX so a
    // trusted peer (or compromised key) cannot exhaust memory.
    let mut engine = open_phase_engine();
    let now = 1_000_000u32;
    for i in 0..PENDING_PROPOSALS_MAX {
        let p = Proposal {
            node_id: node(2),
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::ZERO,
            close_time: now,
            prop_seq: i as u32,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };
        engine.peer_proposal_at(p, now);
    }
    assert_eq!(engine.pending_proposals.len(), PENDING_PROPOSALS_MAX);

    // The 1025th proposal must be dropped, not buffered.
    let overflow = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: Hash256::ZERO,
        close_time: now,
        prop_seq: PENDING_PROPOSALS_MAX as u32,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(overflow, now);
    assert_eq!(engine.pending_proposals.len(), PENDING_PROPOSALS_MAX);
}

// --- T19: ProposalTracker dedup tests ---

/// Build a 3-node engine driven into Establish phase, with us=node(1)
/// and a single-tx local set so peer proposals against a *different*
/// tx_set materialise dispute votes we can assert on.
fn dedup_engine(local_set: TxSet, peer_set: TxSet) -> ConsensusEngine<MockAdapter> {
    let unl = make_unl(&[1, 2, 3]);
    let adapter = MockAdapter::with_tx_sets(vec![local_set.clone(), peer_set]);
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(local_set, 100, 1).unwrap();
    engine
}

#[test]
fn duplicate_proposal_does_not_bump_dispute_counters() {
    // Local set has tx1; peer's set is empty so tx1 becomes a dispute
    // when peer's proposal lands. Re-delivering the identical proposal
    // must NOT bump the dispute's nay_count for that peer (the
    // ProposalTracker drops the second call before it reaches
    // create_disputes).
    let tx1 = Hash256::new([0x01; 32]);
    let local_set = TxSet::new(vec![tx1]);
    let peer_set = TxSet::new(vec![]);
    let mut engine = dedup_engine(local_set.clone(), peer_set.clone());

    let p = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: peer_set.hash,
        close_time: 100,
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(p.clone(), 100);
    assert_eq!(engine.peer_positions.len(), 1);
    let nay_after_first = engine
        .disputes()
        .get(&tx1)
        .expect("dispute should exist after first proposal")
        .nay_count();
    assert_eq!(nay_after_first, 1);

    // Replay identical proposal — same node_id, prev_ledger, prop_seq.
    engine.peer_proposal_at(p, 100);

    // No new dispute created, peer_positions unchanged, vote count
    // unchanged (insert into HashMap with same key/value would also
    // be a no-op, but we want to prove create_disputes was skipped).
    assert_eq!(engine.peer_positions.len(), 1);
    assert_eq!(engine.disputes().len(), 1);
    let nay_after_dup = engine.disputes().get(&tx1).unwrap().nay_count();
    assert_eq!(nay_after_dup, nay_after_first);
    // ProposalTracker still holds prop_seq=0.
    assert_eq!(
        engine
            .proposal_tracker
            .get(&node(2), &Hash256::ZERO)
            .unwrap()
            .prop_seq,
        0
    );
}

#[test]
fn lower_prop_seq_proposal_is_rejected() {
    // First accept prop_seq=5 with peer_set_a (empty). Then re-deliver
    // the same node with prop_seq=3 but a *different* tx_set: the
    // ProposalTracker must reject the older seq, leaving peer_positions
    // pinned to the prop_seq=5 entry.
    let tx1 = Hash256::new([0x01; 32]);
    let tx2 = Hash256::new([0x02; 32]);
    let local_set = TxSet::new(vec![tx1]);
    let peer_set_a = TxSet::new(vec![]);
    let peer_set_b = TxSet::new(vec![tx2]);

    let unl = make_unl(&[1, 2, 3]);
    let adapter = MockAdapter::with_tx_sets(vec![
        local_set.clone(),
        peer_set_a.clone(),
        peer_set_b.clone(),
    ]);
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(local_set, 100, 1).unwrap();

    let high = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: peer_set_a.hash,
        close_time: 100,
        prop_seq: 5,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(high, 100);
    assert_eq!(
        engine.peer_positions.get(&node(2)).unwrap().tx_set_hash,
        peer_set_a.hash
    );

    // Older prop_seq carrying a different tx_set: rejected silently.
    let stale = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: peer_set_b.hash,
        close_time: 100,
        prop_seq: 3,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(stale, 100);

    // Stored position must still be the prop_seq=5 / peer_set_a entry.
    let stored = engine.peer_positions.get(&node(2)).unwrap();
    assert_eq!(stored.prop_seq, 5);
    assert_eq!(stored.tx_set_hash, peer_set_a.hash);
    assert_eq!(
        engine
            .proposal_tracker
            .get(&node(2), &Hash256::ZERO)
            .unwrap()
            .prop_seq,
        5
    );
}

#[test]
fn higher_prop_seq_proposal_replaces_existing() {
    // First accept prop_seq=0 with peer_set_a. Then deliver prop_seq=1
    // with peer_set_b — ProposalTracker must accept and the engine's
    // peer_positions entry must rotate to reflect the new tx_set.
    let tx1 = Hash256::new([0x01; 32]);
    let tx2 = Hash256::new([0x02; 32]);
    let local_set = TxSet::new(vec![tx1]);
    let peer_set_a = TxSet::new(vec![]);
    let peer_set_b = TxSet::new(vec![tx2]);

    let unl = make_unl(&[1, 2, 3]);
    let adapter = MockAdapter::with_tx_sets(vec![
        local_set.clone(),
        peer_set_a.clone(),
        peer_set_b.clone(),
    ]);
    let mut engine = ConsensusEngine::new_with_unl(
        adapter,
        node(1),
        Vec::new(),
        ConsensusParams::default(),
        unl,
    );
    engine.start_round(Hash256::ZERO, 1);
    engine.close_ledger(local_set, 100, 1).unwrap();

    let first = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: peer_set_a.hash,
        close_time: 100,
        prop_seq: 0,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(first, 100);
    assert_eq!(engine.peer_positions.get(&node(2)).unwrap().prop_seq, 0);

    let updated = Proposal {
        node_id: node(2),
        public_key: vec![0x02; 33],
        tx_set_hash: peer_set_b.hash,
        close_time: 100,
        prop_seq: 1,
        ledger_seq: 1,
        prev_ledger: Hash256::ZERO,
        signature: None,
    };
    engine.peer_proposal_at(updated, 100);

    let stored = engine.peer_positions.get(&node(2)).unwrap();
    assert_eq!(stored.prop_seq, 1);
    assert_eq!(stored.tx_set_hash, peer_set_b.hash);
    assert_eq!(
        engine
            .proposal_tracker
            .get(&node(2), &Hash256::ZERO)
            .unwrap()
            .prop_seq,
        1
    );
}
