use std::collections::HashMap;
use std::sync::Arc;

use rxrpl_consensus::ConsensusAdapter;
use rxrpl_consensus::types::{Proposal, TxSet, Validation};
use rxrpl_p2p_proto::MessageType;
use rxrpl_primitives::Hash256;
use tokio::sync::mpsc;

use crate::command::OverlayCommand;
use crate::identity::{NodeIdentity, ValidatorIdentity};
use crate::proto_convert;

/// Consensus adapter that bridges sync ConsensusAdapter calls to the async PeerManager.
///
/// Uses `std::sync::RwLock` (not tokio) because ConsensusAdapter methods are sync.
/// Uses `mpsc::UnboundedSender` which is safe to call from sync code.
#[allow(dead_code)]
pub struct NetworkConsensusAdapter {
    cmd_tx: mpsc::UnboundedSender<OverlayCommand>,
    identity: Arc<NodeIdentity>,
    /// When set, proposals are signed with the validator's **ephemeral signing
    /// key** instead of the node's peer-to-peer key. rippled's UNL contains
    /// validator master keys (with the signing key bound via manifest), not
    /// node peer keys — proposals signed with the node key arrive at rippled
    /// with a `node_pub_key` that is not in any trusted set and get dropped
    /// (issue #76 root cause for `laggards: N`).
    validator_identity: Option<Arc<ValidatorIdentity>>,
    tx_sets: Arc<std::sync::RwLock<HashMap<Hash256, TxSet>>>,
}

impl NetworkConsensusAdapter {
    pub fn new(cmd_tx: mpsc::UnboundedSender<OverlayCommand>, identity: Arc<NodeIdentity>) -> Self {
        Self {
            cmd_tx,
            identity,
            validator_identity: None,
            tx_sets: Arc::new(std::sync::RwLock::new(HashMap::new())),
        }
    }

    /// Attach a `ValidatorIdentity` so that proposals are signed with the
    /// ephemeral signing key (bound to the master key by the manifest).
    /// Without this, rxrpl signs proposals with the node peer key and rippled
    /// silently drops them as untrusted (see field doc above).
    pub fn with_validator_identity(mut self, vid: Arc<ValidatorIdentity>) -> Self {
        self.validator_identity = Some(vid);
        self
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
        // When operating as a UNL validator, the proposal MUST be signed by
        // the manifest-bound signing key and MUST carry the matching
        // public key in `node_pub_key`; otherwise rippled treats the proposal
        // as coming from an unknown node and drops it. Fall back to the node
        // identity for nodes without a validator config (peers, observers).
        match self.validator_identity.as_ref() {
            Some(vid) => {
                signed.public_key = vid.signing_pubkey().as_bytes().to_vec();
                vid.sign_proposal(&mut signed);
            }
            None => {
                self.identity.sign_proposal(&mut signed);
            }
        }
        let payload = proto_convert::encode_propose_set(&signed);
        self.broadcast(MessageType::ProposeSet, payload);
    }

    fn share_position(&self, proposal: &Proposal) {
        self.propose(proposal);

        // Broadcast HaveTransactionSet so peers know we have this tx-set.
        let have_set_payload = proto_convert::encode_have_set(
            &proposal.tx_set_hash,
            1, // tsNEW_SET
        );
        self.broadcast(MessageType::HaveSet, have_set_payload);
    }

    fn share_tx(&self, tx_hash: &Hash256, tx_data: &[u8]) {
        let payload = proto_convert::encode_transaction(tx_hash, tx_data);
        self.broadcast(MessageType::Transaction, payload);
    }

    fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet> {
        self.tx_sets.read().unwrap().get(hash).cloned()
    }

    fn publish_tx_set(&self, tx_set: &TxSet) {
        // Make the candidate set retrievable by peers that receive our
        // ProposeSet; `handle_get_tx_set` serves directly from this cache.
        self.tx_sets
            .write()
            .unwrap()
            .insert(tx_set.hash, tx_set.clone());
    }

    fn on_close(&self, ledger_hash: &Hash256, ledger_seq: u32, _close_time: u32, tx_set: &TxSet) {
        // Cache the tx_set so peers can acquire it
        self.tx_sets
            .write()
            .unwrap()
            .insert(tx_set.hash, tx_set.clone());

        // Broadcast status change. We advertise the genesis ledger as the
        // start of our complete range — current rxrpl nodes never prune
        // history, so any closed ledger from 1..=ledger_seq is locally
        // available. Without this range, late-joining rippled never asks
        // rxrpl for ancestors and its `complete_ledgers` stays empty.
        let payload = proto_convert::encode_status_change(ledger_hash, ledger_seq, 1, ledger_seq);
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
            public_key: vec![0x02; 33],
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

    #[test]
    fn share_position_broadcasts_have_set() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let identity = Arc::new(NodeIdentity::generate());
        let adapter = NetworkConsensusAdapter::new(cmd_tx, identity);

        let proposal = Proposal {
            node_id: NodeId(Hash256::new([0x01; 32])),
            public_key: vec![0x02; 33],
            tx_set_hash: Hash256::new([0x02; 32]),
            close_time: 100,
            prop_seq: 1,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };

        adapter.share_position(&proposal);

        // First message: ProposeSet broadcast
        let cmd1 = cmd_rx.try_recv().unwrap();
        match cmd1 {
            OverlayCommand::Broadcast { msg_type, .. } => {
                assert_eq!(msg_type, MessageType::ProposeSet);
            }
            _ => panic!("expected ProposeSet Broadcast"),
        }

        // Second message: HaveSet broadcast
        let cmd2 = cmd_rx.try_recv().unwrap();
        match cmd2 {
            OverlayCommand::Broadcast { msg_type, payload } => {
                assert_eq!(msg_type, MessageType::HaveSet);
                // Verify the payload decodes correctly
                let have_set = proto_convert::decode_have_set(&payload).unwrap();
                assert_eq!(have_set.hash, proposal.tx_set_hash);
                assert_eq!(have_set.status, 1); // tsNEW_SET
            }
            _ => panic!("expected HaveSet Broadcast"),
        }
    }

    #[test]
    fn tx_sets_shared_ref() {
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let identity = Arc::new(NodeIdentity::generate());
        let adapter = NetworkConsensusAdapter::new(cmd_tx, identity);

        // Get a clone of the shared cache
        let cache = Arc::clone(adapter.tx_sets());

        // Insert directly into the shared cache (simulates overlay layer inserting)
        let tx_set = TxSet::new(vec![Hash256::new([0x05; 32])]);
        let hash = tx_set.hash;
        cache.write().unwrap().insert(hash, tx_set.clone());

        // Adapter should see the set via acquire_tx_set
        let acquired = adapter.acquire_tx_set(&hash).unwrap();
        assert_eq!(acquired.hash, hash);
        assert_eq!(acquired.len(), 1);
    }
}
