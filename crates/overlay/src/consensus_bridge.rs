use std::collections::HashMap;
use std::sync::Arc;

use rxrpl_consensus::ConsensusAdapter;
use rxrpl_consensus::types::{Proposal, TxSet, Validation};
use rxrpl_p2p_proto::MessageType;
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;

use crate::command::OverlayCommand;
use crate::identity::NodeIdentity;
use crate::proto_convert;

/// Consensus adapter that bridges sync ConsensusAdapter calls to the async PeerManager.
///
/// Uses `std::sync::RwLock` (not tokio) because ConsensusAdapter methods are sync.
/// Uses `mpsc::UnboundedSender` which is safe to call from sync code.
#[allow(dead_code)]
pub struct NetworkConsensusAdapter {
    cmd_tx: mpsc::UnboundedSender<OverlayCommand>,
    identity: Arc<NodeIdentity>,
    tx_sets: Arc<std::sync::RwLock<HashMap<Hash256, TxSet>>>,
}

impl NetworkConsensusAdapter {
    pub fn new(cmd_tx: mpsc::UnboundedSender<OverlayCommand>, identity: Arc<NodeIdentity>) -> Self {
        Self {
            cmd_tx,
            identity,
            tx_sets: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Get a reference to the tx_sets cache for external insertion
    /// (e.g., when receiving tx sets from the network).
    pub fn tx_sets(&self) -> &Arc<std::sync::RwLock<HashMap<Hash256, TxSet>>> {
        &self.tx_sets
    }

    fn broadcast(&self, msg_type: MessageType, payload: Vec<u8>) {
        let _ = self
            .cmd_tx
            .send(OverlayCommand::Broadcast { msg_type, payload });
    }
}

impl ConsensusAdapter for NetworkConsensusAdapter {
    fn propose(&self, proposal: &Proposal) {
        let mut signed = proposal.clone();
        self.identity.sign_proposal(&mut signed);
        let payload = proto_convert::encode_propose_set(&signed);
        self.broadcast(MessageType::ProposeSet, payload);
    }

    fn share_position(&self, proposal: &Proposal) {
        self.propose(proposal);
    }

    fn share_tx(&self, tx_hash: &Hash256, tx_data: &[u8]) {
        let payload = proto_convert::encode_transaction(tx_hash, tx_data);
        self.broadcast(MessageType::Transaction, payload);
    }

    fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet> {
        self.tx_sets.read().unwrap().get(hash).cloned()
    }

    fn on_close(&self, ledger_hash: &Hash256, ledger_seq: u32, _close_time: u32, tx_set: &TxSet) {
        // Cache the tx_set so peers can acquire it
        self.tx_sets
            .write()
            .unwrap()
            .insert(tx_set.hash, tx_set.clone());

        // Broadcast status change
        let payload = proto_convert::encode_status_change(ledger_hash, ledger_seq);
        self.broadcast(MessageType::StatusChange, payload);
    }

    fn on_accept(&self, _validation: &Validation) {
        // Validation is broadcast from close_consensus_round after the real
        // ledger hash is computed. The hash is not available at this point
        // because on_accept_ledger returns a sentinel zero.
    }

    fn on_accept_ledger(&self, _tx_set: &TxSet, _close_time: u32, _close_flags: u8) -> Hash256 {
        // Sentinel: the node's close loop handles actual ledger mutation.
        Hash256::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::NodeId;

    #[test]
    fn propose_sends_broadcast() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let identity = Arc::new(NodeIdentity::generate());
        let adapter = NetworkConsensusAdapter::new(cmd_tx, identity);

        let proposal = Proposal {
            node_id: NodeId(Hash256::new([0x01; 32])),
            tx_set_hash: Hash256::new([0x02; 32]),
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };

        adapter.propose(&proposal);

        let cmd = cmd_rx.try_recv().unwrap();
        match cmd {
            OverlayCommand::Broadcast { msg_type, .. } => {
                assert_eq!(msg_type, MessageType::ProposeSet);
            }
            _ => panic!("expected Broadcast"),
        }
    }

    #[test]
    fn tx_set_cache() {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let identity = Arc::new(NodeIdentity::generate());
        let adapter = NetworkConsensusAdapter::new(cmd_tx, identity);

        let tx_set = TxSet::new(vec![Hash256::new([0x01; 32])]);
        let hash = tx_set.hash;

        // Not cached yet
        assert!(adapter.acquire_tx_set(&hash).is_none());

        // Cache via on_close
        adapter.on_close(&Hash256::ZERO, 1, 0, &tx_set);

        // Now cached
        let acquired = adapter.acquire_tx_set(&hash).unwrap();
        assert_eq!(acquired.hash, hash);
    }
}
