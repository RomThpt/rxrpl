use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::adapter::ConsensusAdapter;
use crate::error::ConsensusError;
use crate::params::ConsensusParams;
use crate::phase::ConsensusPhase;
use crate::types::{DisputedTx, NodeId, Proposal, TxSet};

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
}

impl<A: ConsensusAdapter> ConsensusEngine<A> {
    pub fn new(adapter: A, node_id: NodeId, params: ConsensusParams) -> Self {
        Self {
            adapter,
            params,
            phase: ConsensusPhase::Open,
            our_position: None,
            peer_positions: HashMap::new(),
            disputes: HashMap::new(),
            round: 0,
            prev_ledger: Hash256::ZERO,
            node_id,
        }
    }

    /// Get the current consensus phase.
    pub fn phase(&self) -> ConsensusPhase {
        self.phase
    }

    /// Start a new consensus round for the next ledger.
    pub fn start_round(&mut self, prev_ledger: Hash256, ledger_seq: u32) {
        self.phase = ConsensusPhase::Open;
        self.our_position = None;
        self.peer_positions.clear();
        self.disputes.clear();
        self.round = 0;
        self.prev_ledger = prev_ledger;
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
            tx_set_hash: our_set.hash,
            close_time,
            prop_seq: 0,
            ledger_seq,
            prev_ledger: self.prev_ledger,
        };

        self.adapter.propose(&proposal);
        self.our_position = Some(proposal);
        self.phase = ConsensusPhase::Establish;
        Ok(())
    }

    /// Receive a proposal from a peer.
    pub fn peer_proposal(&mut self, proposal: Proposal) {
        if self.phase != ConsensusPhase::Establish {
            return;
        }
        self.peer_positions.insert(proposal.node_id, proposal);
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

        // Count agreement on our position
        let our_hash = match &self.our_position {
            Some(p) => p.tx_set_hash,
            None => return false,
        };

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
            self.phase = ConsensusPhase::Accepted;
            return true;
        }

        self.round += 1;

        // If we've exceeded max rounds, accept anyway
        if self.round >= self.params.max_consensus_rounds {
            self.phase = ConsensusPhase::Accepted;
            return true;
        }

        false
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

#[cfg(test)]
mod tests {
    use super::*;

    struct MockAdapter;

    impl ConsensusAdapter for MockAdapter {
        fn propose(&self, _: &Proposal) {}
        fn share_position(&self, _: &Proposal) {}
        fn share_tx(&self, _: &Hash256, _: &[u8]) {}
        fn acquire_tx_set(&self, _: &Hash256) -> Option<TxSet> {
            None
        }
        fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}
        fn on_accept(&self, _: &crate::types::Validation) {}
    }

    fn test_engine() -> ConsensusEngine<MockAdapter> {
        let node_id = NodeId(Hash256::new([0x01; 32]));
        ConsensusEngine::new(MockAdapter, node_id, ConsensusParams::default())
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
}
