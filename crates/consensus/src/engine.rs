use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::error::ConsensusError;
use crate::params::ConsensusParams;
use crate::phase::ConsensusPhase;
use crate::types::{DisputedTx, NodeId, Proposal, TxSet, Validation};
use crate::unl::TrustedValidatorList;

/// Threshold percentage of trusted validators referencing a different
/// `prev_ledger` before we consider switching chains.
const WRONG_PREV_LEDGER_THRESHOLD: u32 = 60;

/// Result of checking whether trusted peers disagree on prev_ledger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrongPrevLedgerDetected {
    /// The prev_ledger hash that the supermajority of trusted peers reference.
    pub preferred_ledger: Hash256,
    /// How many trusted peers reference that ledger.
    pub peer_count: usize,
    /// Total trusted peers that sent proposals (with any prev_ledger).
    pub total_trusted: usize,
}

/// The RPCA (Ripple Protocol Consensus Algorithm) engine.
///
/// Implements the consensus state machine:
/// - Open: collect transactions
/// - Establish: converge on a transaction set
/// - Accepted: ledger accepted, transition to next round
pub struct ConsensusEngine<A: ConsensusAdapter> {
    adapter: A,
    params: ConsensusParams,
    phase: ConsensusPhase,
    /// Our current proposal.
    our_position: Option<Proposal>,
    /// Our current transaction set.
    our_set: Option<TxSet>,
    /// Proposals from other validators.
    peer_positions: HashMap<NodeId, Proposal>,
    /// Disputed transactions (tx_hash -> dispute).
    disputes: HashMap<Hash256, DisputedTx>,
    /// Current consensus round.
    round: u32,
    /// Previous ledger hash.
    prev_ledger: Hash256,
    /// Our node ID.
    node_id: NodeId,
    /// Our raw public key bytes.
    public_key: Vec<u8>,
    /// Accepted close time after negotiation.
    accepted_close_time: Option<u32>,
    /// Close flags (1 = peers disagree on close time).
    accepted_close_flags: u8,
    /// Accepted validation after consensus.
    accepted_validation: Option<Validation>,
    /// Trusted validator list (UNL). Empty = solo mode.
    unl: TrustedValidatorList,
    /// Proposals received while not in Establish phase, replayed on close.
    pending_proposals: Vec<Proposal>,
    /// Tracks proposals from trusted peers that reference a different
    /// `prev_ledger` than ours. Keyed by (node_id -> their prev_ledger).
    /// Used to detect when we are on the wrong chain.
    wrong_prev_ledger_votes: HashMap<NodeId, Hash256>,
}

impl<A: ConsensusAdapter> ConsensusEngine<A> {
    pub fn new(adapter: A, node_id: NodeId, params: ConsensusParams) -> Self {
        Self::new_with_unl(adapter, node_id, Vec::new(), params, TrustedValidatorList::empty())
    }

    pub fn new_with_unl(
        adapter: A,
        node_id: NodeId,
        public_key: Vec<u8>,
        params: ConsensusParams,
        unl: TrustedValidatorList,
    ) -> Self {
        Self {
            adapter,
            params,
            phase: ConsensusPhase::Open,
            our_position: None,
            our_set: None,
            peer_positions: HashMap::new(),
            disputes: HashMap::new(),
            round: 0,
            prev_ledger: Hash256::ZERO,
            node_id,
            public_key,
            accepted_close_time: None,
            accepted_close_flags: 0,
            accepted_validation: None,
            unl,
            pending_proposals: Vec::new(),
            wrong_prev_ledger_votes: HashMap::new(),
        }
    }

    /// Get a mutable reference to the adapter (for simulation/testing).
    pub fn adapter_mut(&mut self) -> &mut A {
        &mut self.adapter
    }

    /// Get the UNL.
    pub fn unl(&self) -> &TrustedValidatorList {
        &self.unl
    }

    /// Get the current consensus phase.
    pub fn phase(&self) -> ConsensusPhase {
        self.phase
    }

    /// Get a reference to our current position.
    pub fn our_position(&self) -> Option<&Proposal> {
        self.our_position.as_ref()
    }

    /// Get a reference to our current transaction set.
    pub fn our_set(&self) -> Option<&TxSet> {
        self.our_set.as_ref()
    }

    /// Get the accepted close time.
    pub fn accepted_close_time(&self) -> Option<u32> {
        self.accepted_close_time
    }

    /// Get the accepted close flags.
    pub fn accepted_close_flags(&self) -> u8 {
        self.accepted_close_flags
    }

    /// Get the accepted validation.
    pub fn accepted_validation(&self) -> Option<&Validation> {
        self.accepted_validation.as_ref()
    }

    /// Get the current previous ledger hash.
    pub fn prev_ledger(&self) -> Hash256 {
        self.prev_ledger
    }

    /// Get our node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Check whether a supermajority of trusted validators reference a
    /// different `prev_ledger` than ours.
    ///
    /// Returns `Some(WrongPrevLedgerDetected)` when more than 60% of the
    /// trusted peers that sent proposals disagree with our `prev_ledger`.
    /// The caller should abort the current round and switch to the
    /// preferred ledger.
    ///
    /// In solo mode (empty UNL) this always returns `None`.
    pub fn check_wrong_prev_ledger(&self) -> Option<WrongPrevLedgerDetected> {
        if self.unl.is_empty() {
            return None;
        }

        if self.wrong_prev_ledger_votes.is_empty() {
            return None;
        }

        // Count only trusted peers among those who sent mismatched proposals.
        let mut ledger_counts: HashMap<Hash256, usize> = HashMap::new();
        for (node_id, their_prev) in &self.wrong_prev_ledger_votes {
            if self.unl.is_trusted(node_id) {
                *ledger_counts.entry(*their_prev).or_default() += 1;
            }
        }

        if ledger_counts.is_empty() {
            return None;
        }

        // Find the most popular alternative prev_ledger.
        let (preferred, &count) = ledger_counts
            .iter()
            .max_by_key(|&(_, &c)| c)
            .unwrap();

        // Total trusted proposals = those agreeing with us + those disagreeing.
        let agreeing_trusted = self
            .peer_positions
            .values()
            .filter(|p| self.unl.is_trusted(&p.node_id))
            .count();
        let total_trusted = agreeing_trusted + ledger_counts.values().sum::<usize>();

        if total_trusted == 0 {
            return None;
        }

        let pct = (count as u32 * 100) / total_trusted as u32;
        if pct >= WRONG_PREV_LEDGER_THRESHOLD {
            Some(WrongPrevLedgerDetected {
                preferred_ledger: *preferred,
                peer_count: count,
                total_trusted,
            })
        } else {
            None
        }
    }

    /// Get the disputes map.
    pub fn disputes(&self) -> &HashMap<Hash256, DisputedTx> {
        &self.disputes
    }

    /// Start a new consensus round for the next ledger.
    pub fn start_round(&mut self, prev_ledger: Hash256, ledger_seq: u32) {
        self.phase = ConsensusPhase::Open;
        self.our_position = None;
        self.our_set = None;
        self.peer_positions.clear();
        self.disputes.clear();
        self.round = 0;
        self.prev_ledger = prev_ledger;
        self.accepted_close_time = None;
        self.accepted_close_flags = 0;
        self.accepted_validation = None;
        self.wrong_prev_ledger_votes.clear();
        // Note: pending_proposals is NOT cleared here -- they will be
        // replayed in close_ledger() which follows start_round().
        let _ = ledger_seq;
    }

    /// Close the open phase and begin establishing consensus.
    ///
    /// `our_set` is our proposed transaction set.
    pub fn close_ledger(
        &mut self,
        our_set: TxSet,
        close_time: u32,
        ledger_seq: u32,
    ) -> Result<(), ConsensusError> {
        if self.phase != ConsensusPhase::Open {
            return Err(ConsensusError::WrongPhase {
                expected: "open".into(),
                actual: self.phase.to_string(),
            });
        }

        let proposal = Proposal {
            node_id: self.node_id,
            public_key: self.public_key.clone(),
            tx_set_hash: our_set.hash,
            close_time,
            prop_seq: 0,
            ledger_seq,
            prev_ledger: self.prev_ledger,
            signature: None,
        };

        self.adapter.propose(&proposal);
        self.our_position = Some(proposal);
        self.our_set = Some(our_set);
        self.phase = ConsensusPhase::Establish;

        // Replay any proposals buffered while we were in Open phase
        let pending = std::mem::take(&mut self.pending_proposals);
        for p in pending {
            self.peer_proposal(p);
        }

        Ok(())
    }

    /// Receive a proposal from a peer.
    pub fn peer_proposal(&mut self, proposal: Proposal) {
        if self.phase != ConsensusPhase::Establish {
            // Buffer proposals received outside Establish phase
            self.pending_proposals.push(proposal);
            return;
        }

        // UNL filtering: if UNL is non-empty, only accept trusted nodes
        if !self.unl.is_empty() && !self.unl.is_trusted(&proposal.node_id) {
            tracing::debug!("rejected proposal from untrusted {:?}", proposal.node_id);
            return;
        }

        // Track proposals referencing a different previous ledger.
        // These are rejected from normal consensus but counted toward
        // wrong-prev-ledger detection so we can switch chains if needed.
        if proposal.prev_ledger != self.prev_ledger {
            tracing::debug!(
                "rejected proposal: prev_ledger mismatch (ours={}, theirs={}), tracking for recovery",
                self.prev_ledger, proposal.prev_ledger
            );
            self.wrong_prev_ledger_votes
                .insert(proposal.node_id, proposal.prev_ledger);
            return;
        }

        // Reject proposals for a different ledger sequence
        if let Some(ref our) = self.our_position {
            if proposal.ledger_seq != our.ledger_seq {
                tracing::debug!(
                    "rejected proposal: seq mismatch (ours={}, theirs={})",
                    our.ledger_seq, proposal.ledger_seq
                );
                return;
            }
        }

        tracing::debug!("accepted proposal from {:?} seq={}", proposal.node_id, proposal.ledger_seq);
        let node_id = proposal.node_id;
        self.peer_positions.insert(node_id, proposal);
        self.create_disputes();
    }

    /// Create or update disputes from peer proposals that differ from ours.
    fn create_disputes(&mut self) {
        let our_set = match &self.our_set {
            Some(s) => s,
            None => return,
        };

        for (node_id, peer_prop) in &self.peer_positions {
            if peer_prop.tx_set_hash == our_set.hash {
                // Same set, vote yay on all our txs for this peer
                for tx_hash in &our_set.txs {
                    if let Some(dispute) = self.disputes.get_mut(tx_hash) {
                        dispute.vote(*node_id, true);
                    }
                }
                continue;
            }

            // Try to acquire peer's tx set
            let peer_set = match self.adapter.acquire_tx_set(&peer_prop.tx_set_hash) {
                Some(s) => s,
                None => continue,
            };

            // Find txs in our set but not peer's
            for tx_hash in &our_set.txs {
                if !peer_set.txs.contains(tx_hash) {
                    let dispute = self
                        .disputes
                        .entry(*tx_hash)
                        .or_insert_with(|| DisputedTx::new(*tx_hash, true));
                    dispute.vote(*node_id, false);
                }
            }

            // Find txs in peer's set but not ours
            for tx_hash in &peer_set.txs {
                if !our_set.txs.contains(tx_hash) {
                    let dispute = self
                        .disputes
                        .entry(*tx_hash)
                        .or_insert_with(|| DisputedTx::new(*tx_hash, false));
                    dispute.vote(*node_id, true);
                }
            }

            // Txs in both sets: peer agrees
            for tx_hash in &our_set.txs {
                if peer_set.txs.contains(tx_hash) {
                    if let Some(dispute) = self.disputes.get_mut(tx_hash) {
                        dispute.vote(*node_id, true);
                    }
                }
            }
        }
    }

    /// Compute the effective close time from all proposals (median, rounded).
    fn effective_close_time(&self) -> u32 {
        let our_time = match &self.our_position {
            Some(p) => p.close_time,
            None => return 0,
        };

        if self.peer_positions.is_empty() {
            return our_time;
        }

        let mut times: Vec<u32> = vec![our_time];
        for peer in self.peer_positions.values() {
            times.push(peer.close_time);
        }
        times.sort();

        let median = times[times.len() / 2];
        round_close_time(median, self.params.close_time_resolution)
    }

    /// Run one round of convergence.
    ///
    /// Adjusts our position based on peer proposals and the current threshold.
    /// Returns `true` if consensus has been reached.
    pub fn converge(&mut self) -> bool {
        if self.phase != ConsensusPhase::Establish {
            return false;
        }

        let threshold = self.params.threshold_for_round(self.round);

        // Resolve disputes and update our set if needed
        let mut set_changed = false;
        if let Some(ref mut our_set) = self.our_set {
            let mut new_txs = our_set.txs.clone();

            for dispute in self.disputes.values() {
                let should_include = dispute.should_include(threshold);
                if should_include != dispute.our_vote {
                    set_changed = true;
                    if should_include {
                        // Add tx we didn't have
                        if !new_txs.contains(&dispute.tx_hash) {
                            new_txs.push(dispute.tx_hash);
                        }
                    } else {
                        // Remove tx we had
                        new_txs.retain(|h| h != &dispute.tx_hash);
                    }
                }
            }

            if set_changed {
                *our_set = TxSet::new(new_txs);
                if let Some(ref mut pos) = self.our_position {
                    pos.tx_set_hash = our_set.hash;
                    pos.prop_seq += 1;
                    self.adapter.share_position(pos);
                }
                // Update dispute our_vote to match new reality
                for dispute in self.disputes.values_mut() {
                    let in_set = our_set.txs.contains(&dispute.tx_hash);
                    dispute.our_vote = in_set;
                }
            }
        }

        // Count agreement on our position
        let our_hash = match &self.our_position {
            Some(p) => p.tx_set_hash,
            None => return false,
        };

        if self.unl.is_empty() {
            // Solo mode: percentage-based agreement (original logic)
            let total = self.peer_positions.len() + 1; // +1 for us
            let agreeing = self
                .peer_positions
                .values()
                .filter(|p| p.tx_set_hash == our_hash)
                .count()
                + 1; // +1 for us

            let agreement_pct = if total > 0 {
                (agreeing as u32 * 100) / total as u32
            } else {
                100
            };

            if agreement_pct >= threshold {
                self.accept();
                return true;
            }
        } else {
            // UNL mode: count only trusted members who agree
            let agreeing_unl = self
                .peer_positions
                .values()
                .filter(|p| self.unl.is_trusted(&p.node_id) && p.tx_set_hash == our_hash)
                .count();
            let self_counts = if self.unl.is_trusted(&self.node_id) {
                1
            } else {
                0
            };
            if agreeing_unl + self_counts >= self.unl.quorum_threshold() {
                self.accept();
                return true;
            }
        }

        self.round += 1;

        // If we've exceeded max rounds, accept anyway
        if self.round >= self.params.max_consensus_rounds {
            self.accept();
            return true;
        }

        false
    }

    /// Accept the consensus result: compute close time, create validation,
    /// notify adapter.
    fn accept(&mut self) {
        self.phase = ConsensusPhase::Accepted;

        // Compute effective close time
        let close_time = self.effective_close_time();
        self.accepted_close_time = Some(close_time);

        // Check if peers disagree on close time
        if !self.peer_positions.is_empty() {
            let our_time = self
                .our_position
                .as_ref()
                .map(|p| p.close_time)
                .unwrap_or(0);
            let mut times: Vec<u32> = vec![our_time];
            for peer in self.peer_positions.values() {
                times.push(peer.close_time);
            }
            let min = *times.iter().min().unwrap();
            let max = *times.iter().max().unwrap();
            if max - min > self.params.close_time_resolution {
                self.accepted_close_flags = 1;
            }
        }

        // Ask adapter to apply the tx set and get ledger hash
        let our_set = match &self.our_set {
            Some(s) => s.clone(),
            None => return,
        };

        let ledger_hash =
            self.adapter
                .on_accept_ledger(&our_set, close_time, self.accepted_close_flags);

        let ledger_seq = self
            .our_position
            .as_ref()
            .map(|p| p.ledger_seq)
            .unwrap_or(0);

        // Create validation
        let validation = Validation {
            node_id: self.node_id,
            public_key: self.public_key.clone(),
            ledger_hash,
            ledger_seq,
            full: true,
            close_time,
            sign_time: close_time,
            signature: None,
            amendments: vec![],
        };

        self.adapter.on_accept(&validation);
        self.accepted_validation = Some(validation);
    }

    /// Get the accepted transaction set hash, if consensus was reached.
    pub fn accepted_set(&self) -> Option<Hash256> {
        if self.phase == ConsensusPhase::Accepted {
            self.our_position.as_ref().map(|p| p.tx_set_hash)
        } else {
            None
        }
    }
}

/// Round a close time to the nearest resolution boundary.
pub fn round_close_time(t: u32, resolution: u32) -> u32 {
    if resolution == 0 {
        return t;
    }
    (t + resolution / 2) / resolution * resolution
}

#[cfg(test)]
mod tests {
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
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_bc.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set_b.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::new([0xFF; 32]), // unknown
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });
        engine.peer_proposal(Proposal {
            node_id: node_c,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 150,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

        assert!(engine.converge());
        // Median of [100, 150, 200] = 150, rounded to 30s = 150 (already aligned)
        assert_eq!(engine.accepted_close_time(), Some(150));
    }

    #[test]
    fn round_close_time_function() {
        assert_eq!(round_close_time(145, 30), 150);
        assert_eq!(round_close_time(150, 30), 150);
        assert_eq!(round_close_time(130, 30), 120);
        assert_eq!(round_close_time(100, 0), 100);
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

        // Peer proposes time far away (spread > 30s resolution)
        engine.peer_proposal(Proposal {
            node_id: node_b,
            public_key: vec![0x02; 33],
            tx_set_hash: set.hash,
            close_time: 200,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        });

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

    fn proposal_for(
        node_id: NodeId,
        tx_set_hash: Hash256,
        prev_ledger: Hash256,
        seq: u32,
    ) -> Proposal {
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
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Node 99 is not trusted
        engine.peer_proposal(proposal_for(node(99), set.hash, Hash256::ZERO, 1));
        assert!(engine.peer_positions.is_empty());
    }

    #[test]
    fn trusted_proposal_accepted_with_unl() {
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Node 2 is trusted
        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
        assert_eq!(engine.peer_positions.len(), 1);
    }

    #[test]
    fn mismatched_prev_ledger_ignored() {
        let unl = make_unl(&[1, 2]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Different prev_ledger
        let bad_prev = Hash256::new([0xFF; 32]);
        engine.peer_proposal(proposal_for(node(2), set.hash, bad_prev, 1));
        assert!(engine.peer_positions.is_empty());
    }

    #[test]
    fn unl_quorum_not_met_does_not_accept() {
        // 5-node UNL, quorum = ceil(5*0.8) = 4
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        // Only nodes 2, 3 agree (+ us = 3 total, need 4)
        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));

        assert!(!engine.converge());
        assert_eq!(engine.phase(), ConsensusPhase::Establish);
    }

    #[test]
    fn unl_quorum_met_accepts() {
        // 5-node UNL, quorum = 4. Us (node 1) + nodes 2, 3, 4 = 4.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine =
            ConsensusEngine::new_with_unl(adapter, node(1), Vec::new(), ConsensusParams::default(), unl);
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        engine.peer_proposal(proposal_for(node(2), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, Hash256::ZERO, 1));

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
        engine.peer_proposal(proposal_for(node(2), set.hash, bad_prev, 1));

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_detected_with_supermajority() {
        // 5-node UNL. 4 peers send proposals with a different prev_ledger.
        // 4/4 trusted disagree = 100% > 60% threshold.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        let our_prev = Hash256::ZERO;
        engine.start_round(our_prev, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, peer_prev, 1));

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
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // 1 peer disagrees
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        // 3 peers agree
        engine.peer_proposal(proposal_for(node(3), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, Hash256::ZERO, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, Hash256::ZERO, 1));

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_at_exact_threshold() {
        // 5-node UNL. 3 peers disagree, 2 agree.
        // 3/5 = 60% >= 60%, should trigger.
        let unl = make_unl(&[1, 2, 3, 4, 5]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // 3 peers disagree
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, peer_prev, 1));
        // 2 peers agree
        engine.peer_proposal(proposal_for(node(5), set.hash, Hash256::ZERO, 1));

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
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        // Untrusted nodes disagree
        engine.peer_proposal(proposal_for(node(50), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(51), set.hash, peer_prev, 1));

        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }

    #[test]
    fn wrong_prev_ledger_cleared_on_new_round() {
        // After start_round, wrong_prev_ledger tracking resets.
        let unl = make_unl(&[1, 2, 3]);
        let adapter = SimpleAdapter;
        let mut engine = ConsensusEngine::new_with_unl(
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let peer_prev = Hash256::new([0xBB; 32]);
        engine.peer_proposal(proposal_for(node(2), set.hash, peer_prev, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, peer_prev, 1));

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
            adapter, node(1), Vec::new(), ConsensusParams::default(), unl,
        );
        engine.start_round(Hash256::ZERO, 1);

        let set = TxSet::new(vec![]);
        engine.close_ledger(set.clone(), 100, 1).unwrap();

        let prev_a = Hash256::new([0xAA; 32]);
        let prev_b = Hash256::new([0xBB; 32]);
        engine.peer_proposal(proposal_for(node(2), set.hash, prev_a, 1));
        engine.peer_proposal(proposal_for(node(3), set.hash, prev_a, 1));
        engine.peer_proposal(proposal_for(node(4), set.hash, prev_b, 1));
        engine.peer_proposal(proposal_for(node(5), set.hash, prev_b, 1));

        // 2/4 = 50% < 60% -> no detection
        assert_eq!(engine.check_wrong_prev_ledger(), None);
    }
}
