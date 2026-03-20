use std::collections::HashMap;

use rxrpl_primitives::Hash256;

/// Unique identifier for a consensus participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub Hash256);

impl NodeId {
    /// Derive a NodeId from a public key (SHA-512-Half of the key bytes).
    pub fn from_public_key(pk: &[u8]) -> Self {
        let hash = rxrpl_crypto::sha512_half::sha512_half(&[pk]);
        Self(hash)
    }
}

/// A consensus proposal from a validator.
#[derive(Clone, Debug)]
pub struct Proposal {
    /// The proposer's node ID.
    pub node_id: NodeId,
    /// Raw public key bytes (33 bytes for secp256k1).
    pub public_key: Vec<u8>,
    /// Proposed transaction set hash.
    pub tx_set_hash: Hash256,
    /// Target close time (ripple epoch seconds).
    pub close_time: u32,
    /// Proposal sequence (0 = initial, increments on changes).
    pub prop_seq: u32,
    /// Ledger sequence this proposal is for.
    pub ledger_seq: u32,
    /// Previous ledger hash (establishes which ledger we're building on).
    pub prev_ledger: Hash256,
    /// Optional cryptographic signature.
    pub signature: Option<Vec<u8>>,
}

/// A ledger validation from a validator.
#[derive(Clone, Debug)]
pub struct Validation {
    /// The validator's node ID.
    pub node_id: NodeId,
    /// Hash of the validated ledger.
    pub ledger_hash: Hash256,
    /// Sequence of the validated ledger.
    pub ledger_seq: u32,
    /// Whether this is a full validation (vs. partial).
    pub full: bool,
    /// Close time of the validated ledger.
    pub close_time: u32,
    /// Signing time of this validation.
    pub sign_time: u32,
    /// Optional cryptographic signature.
    pub signature: Option<Vec<u8>>,
}

impl Proposal {
    /// Compute the data to be signed (rippled-compatible):
    /// HashPrefix::proposal(4) || prop_seq(4) || close_time(4) || prev_ledger(32) || tx_set_hash(32).
    pub fn signing_data(&self) -> Vec<u8> {
        // HashPrefix::proposal = 'P','R','P',0 = 0x50525000
        const HASH_PREFIX_PROPOSAL: [u8; 4] = [0x50, 0x52, 0x50, 0x00];
        let mut data = Vec::with_capacity(76);
        data.extend_from_slice(&HASH_PREFIX_PROPOSAL);
        data.extend_from_slice(&self.prop_seq.to_be_bytes());
        data.extend_from_slice(&self.close_time.to_be_bytes());
        data.extend_from_slice(self.prev_ledger.as_bytes());
        data.extend_from_slice(self.tx_set_hash.as_bytes());
        data
    }

    /// Sign this proposal with the given private key and key type.
    pub fn sign(&mut self, private_key: &[u8], key_type: rxrpl_crypto::KeyType) {
        let data = self.signing_data();
        let sig = match key_type {
            rxrpl_crypto::KeyType::Secp256k1 => {
                rxrpl_crypto::secp256k1::sign(&data, private_key)
                    .map(|s| s.as_bytes().to_vec())
            }
            rxrpl_crypto::KeyType::Ed25519 => {
                rxrpl_crypto::ed25519::sign(&data, private_key)
                    .map(|s| s.as_bytes().to_vec())
            }
        };
        if let Ok(sig) = sig {
            self.signature = Some(sig);
        }
    }

    /// Verify this proposal's signature against a public key.
    pub fn verify(&self, public_key: &[u8]) -> bool {
        match &self.signature {
            Some(sig) => {
                let data = self.signing_data();
                let is_ed25519 = public_key.first() == Some(&0xED);
                if is_ed25519 {
                    rxrpl_crypto::ed25519::verify(&data, public_key, sig)
                } else {
                    rxrpl_crypto::secp256k1::verify(&data, public_key, sig)
                }
            }
            None => false,
        }
    }
}

impl Validation {
    /// Compute the data to be signed: ledger_hash(32) || ledger_seq(4) || close_time(4) || sign_time(4) || full(1).
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(45);
        data.extend_from_slice(self.ledger_hash.as_bytes());
        data.extend_from_slice(&self.ledger_seq.to_be_bytes());
        data.extend_from_slice(&self.close_time.to_be_bytes());
        data.extend_from_slice(&self.sign_time.to_be_bytes());
        data.push(if self.full { 1 } else { 0 });
        data
    }

    /// Sign this validation with the given private key and key type.
    pub fn sign(&mut self, private_key: &[u8], key_type: rxrpl_crypto::KeyType) {
        let data = self.signing_data();
        let sig = match key_type {
            rxrpl_crypto::KeyType::Secp256k1 => {
                rxrpl_crypto::secp256k1::sign(&data, private_key)
                    .map(|s| s.as_bytes().to_vec())
            }
            rxrpl_crypto::KeyType::Ed25519 => {
                rxrpl_crypto::ed25519::sign(&data, private_key)
                    .map(|s| s.as_bytes().to_vec())
            }
        };
        if let Ok(sig) = sig {
            self.signature = Some(sig);
        }
    }

    /// Verify this validation's signature against a public key.
    pub fn verify(&self, public_key: &[u8]) -> bool {
        match &self.signature {
            Some(sig) => {
                let data = self.signing_data();
                let is_ed25519 = public_key.first() == Some(&0xED);
                if is_ed25519 {
                    rxrpl_crypto::ed25519::verify(&data, public_key, sig)
                } else {
                    rxrpl_crypto::secp256k1::verify(&data, public_key, sig)
                }
            }
            None => false,
        }
    }
}

/// A set of transactions proposed for a ledger.
#[derive(Clone, Debug)]
pub struct TxSet {
    /// Hash of this transaction set.
    pub hash: Hash256,
    /// Transaction hashes in this set.
    pub txs: Vec<Hash256>,
}

impl TxSet {
    pub fn new(txs: Vec<Hash256>) -> Self {
        // Compute hash from sorted tx hashes
        let mut sorted = txs.clone();
        sorted.sort();
        let mut data = Vec::with_capacity(sorted.len() * 32);
        for tx in &sorted {
            data.extend_from_slice(tx.as_bytes());
        }
        let hash = rxrpl_crypto::sha512_half::sha512_half(&[&data]);
        Self { hash, txs: sorted }
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }
}

/// A transaction disputed between proposals.
#[derive(Clone, Debug)]
pub struct DisputedTx {
    /// The transaction hash.
    pub tx_hash: Hash256,
    /// Whether we initially include this tx.
    pub our_vote: bool,
    /// Per-node votes: true = include, false = exclude.
    votes: HashMap<NodeId, bool>,
}

impl DisputedTx {
    /// Create a new disputed transaction.
    pub fn new(tx_hash: Hash256, our_vote: bool) -> Self {
        Self {
            tx_hash,
            our_vote,
            votes: HashMap::new(),
        }
    }

    /// Record a vote from a node.
    pub fn vote(&mut self, node: NodeId, include: bool) {
        self.votes.insert(node, include);
    }

    /// Number of votes to include this tx (not counting ours).
    pub fn yay_count(&self) -> usize {
        self.votes.values().filter(|&&v| v).count()
    }

    /// Number of votes to exclude this tx (not counting ours).
    pub fn nay_count(&self) -> usize {
        self.votes.values().filter(|&&v| !v).count()
    }

    /// Whether we should include this transaction at the given threshold.
    ///
    /// Counts our vote plus all peer votes.
    pub fn should_include(&self, threshold: u32) -> bool {
        let our_yay: usize = if self.our_vote { 1 } else { 0 };
        let yays = self.yay_count() + our_yay;
        let total = self.votes.len() + 1; // +1 for us
        if total == 0 {
            return false;
        }
        (yays as u32 * 100) / total as u32 >= threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_set_deterministic() {
        let tx1 = Hash256::new([0x01; 32]);
        let tx2 = Hash256::new([0x02; 32]);

        let set1 = TxSet::new(vec![tx1, tx2]);
        let set2 = TxSet::new(vec![tx2, tx1]);
        assert_eq!(set1.hash, set2.hash);
    }

    #[test]
    fn disputed_tx_threshold() {
        let mut tx = DisputedTx::new(Hash256::new([0x01; 32]), true);
        // Our vote = true, add 7 more yays and 2 nays => 8/10 = 80%
        for i in 0..7 {
            tx.vote(NodeId(Hash256::new([i + 10; 32])), true);
        }
        tx.vote(NodeId(Hash256::new([0x80; 32])), false);
        tx.vote(NodeId(Hash256::new([0x81; 32])), false);

        assert!(tx.should_include(50)); // 80% >= 50%
        assert!(tx.should_include(80)); // 80% >= 80%
        assert!(!tx.should_include(81)); // 80% < 81%
    }

    #[test]
    fn disputed_tx_vote_counts() {
        let mut tx = DisputedTx::new(Hash256::new([0x01; 32]), false);
        tx.vote(NodeId(Hash256::new([0x10; 32])), true);
        tx.vote(NodeId(Hash256::new([0x11; 32])), true);
        tx.vote(NodeId(Hash256::new([0x12; 32])), false);
        assert_eq!(tx.yay_count(), 2);
        assert_eq!(tx.nay_count(), 1);
        // our_vote=false, so 2 yays / 4 total = 50%
        assert!(tx.should_include(50));
        assert!(!tx.should_include(51));
    }

    #[test]
    fn proposal_sign_verify_roundtrip() {
        let seed = rxrpl_crypto::Seed::from_passphrase("test");
        let kp = rxrpl_crypto::KeyPair::from_seed(&seed, rxrpl_crypto::KeyType::Ed25519);

        let mut proposal = Proposal {
            node_id: NodeId::from_public_key(kp.public_key.as_bytes()),
            public_key: kp.public_key.as_bytes().to_vec(),
            tx_set_hash: Hash256::new([0x01; 32]),
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };

        proposal.sign(&kp.private_key, kp.key_type);
        assert!(proposal.signature.is_some());
        assert!(proposal.verify(kp.public_key.as_bytes()));
    }

    #[test]
    fn proposal_tampered_fails_verify() {
        let seed = rxrpl_crypto::Seed::from_passphrase("test");
        let kp = rxrpl_crypto::KeyPair::from_seed(&seed, rxrpl_crypto::KeyType::Ed25519);

        let mut proposal = Proposal {
            node_id: NodeId::from_public_key(kp.public_key.as_bytes()),
            public_key: kp.public_key.as_bytes().to_vec(),
            tx_set_hash: Hash256::new([0x01; 32]),
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };

        proposal.sign(&kp.private_key, kp.key_type);
        // Tamper
        proposal.close_time = 999;
        assert!(!proposal.verify(kp.public_key.as_bytes()));
    }

    #[test]
    fn validation_sign_verify_roundtrip() {
        let seed = rxrpl_crypto::Seed::from_passphrase("test");
        let kp = rxrpl_crypto::KeyPair::from_seed(&seed, rxrpl_crypto::KeyType::Ed25519);

        let mut validation = Validation {
            node_id: NodeId::from_public_key(kp.public_key.as_bytes()),
            ledger_hash: Hash256::new([0xAA; 32]),
            ledger_seq: 5,
            full: true,
            close_time: 100,
            sign_time: 101,
            signature: None,
        };

        validation.sign(&kp.private_key, kp.key_type);
        assert!(validation.signature.is_some());
        assert!(validation.verify(kp.public_key.as_bytes()));
    }

    #[test]
    fn unsigned_proposal_fails_verify() {
        let seed = rxrpl_crypto::Seed::from_passphrase("test");
        let kp = rxrpl_crypto::KeyPair::from_seed(&seed, rxrpl_crypto::KeyType::Ed25519);

        let proposal = Proposal {
            node_id: NodeId::from_public_key(kp.public_key.as_bytes()),
            public_key: kp.public_key.as_bytes().to_vec(),
            tx_set_hash: Hash256::new([0x01; 32]),
            close_time: 100,
            prop_seq: 0,
            ledger_seq: 1,
            prev_ledger: Hash256::ZERO,
            signature: None,
        };

        assert!(!proposal.verify(kp.public_key.as_bytes()));
    }

    #[test]
    fn node_id_from_public_key() {
        let seed = rxrpl_crypto::Seed::from_passphrase("test");
        let kp = rxrpl_crypto::KeyPair::from_seed(&seed, rxrpl_crypto::KeyType::Ed25519);

        let id1 = NodeId::from_public_key(kp.public_key.as_bytes());
        let id2 = NodeId::from_public_key(kp.public_key.as_bytes());
        assert_eq!(id1, id2);
        assert!(!id1.0.is_zero());
    }
}
