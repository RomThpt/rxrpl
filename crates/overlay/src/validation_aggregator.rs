/// Tracks validation votes per ledger from network peers.
///
/// When enough validators (quorum) agree on a ledger hash for a given
/// sequence, that ledger is considered "validated" by the network.
use std::collections::HashMap;

use rxrpl_consensus::types::Validation;
use rxrpl_primitives::Hash256;

/// Result when a ledger achieves validation quorum.
#[derive(Debug, Clone)]
pub struct ValidatedLedger {
    pub hash: Hash256,
    pub seq: u32,
    pub validation_count: usize,
}

/// Aggregates validations from network peers per ledger.
pub struct ValidationAggregator {
    /// Validations grouped by (ledger_seq, ledger_hash).
    by_ledger: HashMap<(u32, Hash256), Vec<Validation>>,
    /// Track which sequences we've already declared validated.
    validated_seqs: HashMap<u32, Hash256>,
    /// Minimum validations needed to consider a ledger validated.
    /// For an observer with no UNL, we use a simple count threshold.
    min_validations: usize,
    /// Highest validated sequence seen.
    pub highest_validated_seq: u32,
    /// Hash of the highest validated ledger.
    pub highest_validated_hash: Hash256,
}

impl ValidationAggregator {
    pub fn new(min_validations: usize) -> Self {
        Self {
            by_ledger: HashMap::new(),
            validated_seqs: HashMap::new(),
            min_validations: min_validations.max(1),
            highest_validated_seq: 0,
            highest_validated_hash: Hash256::ZERO,
        }
    }

    /// Update the quorum threshold dynamically (e.g. from validator list).
    pub fn update_quorum(&mut self, new_quorum: usize) {
        self.min_validations = new_quorum.max(1);
    }

    /// Add a validation and check if quorum is reached.
    ///
    /// Returns `Some(ValidatedLedger)` if this validation caused the ledger
    /// to reach quorum for the first time.
    pub fn add_validation(&mut self, validation: Validation) -> Option<ValidatedLedger> {
        let seq = validation.ledger_seq;
        let hash = validation.ledger_hash;

        // Skip if we already validated this sequence
        if self.validated_seqs.contains_key(&seq) {
            return None;
        }

        // Only process full validations
        if !validation.full {
            return None;
        }

        let key = (seq, hash);
        let validations = self.by_ledger.entry(key).or_default();

        // Deduplicate by node_id
        if validations.iter().any(|v| v.node_id == validation.node_id) {
            return None;
        }

        validations.push(validation);
        let count = validations.len();

        if count >= self.min_validations {
            self.validated_seqs.insert(seq, hash);

            if seq > self.highest_validated_seq {
                self.highest_validated_seq = seq;
                self.highest_validated_hash = hash;
            }

            // Cleanup old entries (keep only recent 100 sequences)
            self.cleanup(seq);

            return Some(ValidatedLedger {
                hash,
                seq,
                validation_count: count,
            });
        }

        None
    }

    /// Get the number of validations for a specific ledger.
    pub fn validation_count(&self, seq: u32, hash: &Hash256) -> usize {
        self.by_ledger
            .get(&(seq, *hash))
            .map(|v| v.len())
            .unwrap_or(0)
    }

    /// Check if a ledger sequence has been validated.
    pub fn is_validated(&self, seq: u32) -> bool {
        self.validated_seqs.contains_key(&seq)
    }

    /// Get the validated hash for a sequence, if any.
    pub fn validated_hash(&self, seq: u32) -> Option<Hash256> {
        self.validated_seqs.get(&seq).copied()
    }

    /// Remove old entries to prevent unbounded growth.
    fn cleanup(&mut self, current_seq: u32) {
        if current_seq < 100 {
            return;
        }
        let cutoff = current_seq - 100;
        self.by_ledger.retain(|(seq, _), _| *seq > cutoff);
        self.validated_seqs.retain(|seq, _| *seq > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::NodeId;

    fn make_validation(node_byte: u8, seq: u32, hash: Hash256) -> Validation {
        Validation {
            node_id: NodeId(Hash256::new([node_byte; 32])),
            ledger_hash: hash,
            ledger_seq: seq,
            full: true,
            close_time: 100,
            sign_time: 100,
            signature: None,
        }
    }

    #[test]
    fn quorum_reached() {
        let mut agg = ValidationAggregator::new(3);
        let hash = Hash256::new([0xAA; 32]);

        assert!(agg.add_validation(make_validation(1, 10, hash)).is_none());
        assert!(agg.add_validation(make_validation(2, 10, hash)).is_none());
        let result = agg.add_validation(make_validation(3, 10, hash));
        assert!(result.is_some());

        let vl = result.unwrap();
        assert_eq!(vl.seq, 10);
        assert_eq!(vl.hash, hash);
        assert_eq!(vl.validation_count, 3);
    }

    #[test]
    fn duplicate_node_ignored() {
        let mut agg = ValidationAggregator::new(2);
        let hash = Hash256::new([0xBB; 32]);

        agg.add_validation(make_validation(1, 5, hash));
        // Same node again
        assert!(agg.add_validation(make_validation(1, 5, hash)).is_none());
        // Different node reaches quorum
        assert!(agg.add_validation(make_validation(2, 5, hash)).is_some());
    }

    #[test]
    fn different_hashes_counted_separately() {
        let mut agg = ValidationAggregator::new(2);
        let hash_a = Hash256::new([0xAA; 32]);
        let hash_b = Hash256::new([0xBB; 32]);

        agg.add_validation(make_validation(1, 10, hash_a));
        agg.add_validation(make_validation(2, 10, hash_b));
        // Neither reached quorum
        assert_eq!(agg.validation_count(10, &hash_a), 1);
        assert_eq!(agg.validation_count(10, &hash_b), 1);
    }

    #[test]
    fn already_validated_skipped() {
        let mut agg = ValidationAggregator::new(1);
        let hash = Hash256::new([0xCC; 32]);

        assert!(agg.add_validation(make_validation(1, 5, hash)).is_some());
        // Already validated, skip
        assert!(agg.add_validation(make_validation(2, 5, hash)).is_none());
    }

    #[test]
    fn highest_validated_tracked() {
        let mut agg = ValidationAggregator::new(1);
        let hash_5 = Hash256::new([0x55; 32]);
        let hash_10 = Hash256::new([0xAA; 32]);

        agg.add_validation(make_validation(1, 5, hash_5));
        assert_eq!(agg.highest_validated_seq, 5);

        agg.add_validation(make_validation(1, 10, hash_10));
        assert_eq!(agg.highest_validated_seq, 10);
        assert_eq!(agg.highest_validated_hash, hash_10);
    }

    #[test]
    fn non_full_validation_ignored() {
        let mut agg = ValidationAggregator::new(1);
        let mut val = make_validation(1, 5, Hash256::new([0xDD; 32]));
        val.full = false;
        assert!(agg.add_validation(val).is_none());
    }
}
