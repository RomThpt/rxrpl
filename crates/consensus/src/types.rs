use rxrpl_primitives::Hash256;

/// Unique identifier for a consensus participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub Hash256);

/// A consensus proposal from a validator.
#[derive(Clone, Debug)]
pub struct Proposal {
    /// The proposer's node ID.
    pub node_id: NodeId,
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
    /// Number of validators that include this tx.
    pub yays: u32,
    /// Number of validators that exclude this tx.
    pub nays: u32,
}

impl DisputedTx {
    /// Whether we should include this transaction at the given threshold.
    pub fn should_include(&self, threshold: u32) -> bool {
        let total = self.yays + self.nays;
        if total == 0 {
            return false;
        }
        self.yays * 100 / total >= threshold
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
        let tx = DisputedTx {
            tx_hash: Hash256::new([0x01; 32]),
            yays: 8,
            nays: 2,
        };
        assert!(tx.should_include(50)); // 80% >= 50%
        assert!(tx.should_include(80)); // 80% >= 80%
        assert!(!tx.should_include(81)); // 80% < 81%
    }
}
