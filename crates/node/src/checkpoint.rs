//! Checkpoint trust anchor establishment.
//!
//! When a node starts with `--starting-ledger`, it cannot trust an arbitrary
//! peer's word about what the head ledger looks like. Instead it waits for a
//! quorum of UNL-trusted validators to **agree on the hash** for that
//! sequence, and uses that agreed hash as the *trust anchor*.
//!
//! From the trust anchor, the node can fetch the ledger header, walk the
//! [`LedgerHashes`] skip-list backward to reach any earlier ledger, and
//! adopt the resulting state without ever downloading the full chain from
//! genesis.
//!
//! This module deals with the **trust** part. The fetching and skip-list
//! traversal are the caller's responsibility (see `bootstrap` in
//! `crates/node/src/node.rs`).

use std::collections::HashMap;

use rxrpl_consensus::types::Validation;
use rxrpl_primitives::{Hash256, PublicKey};

/// Configuration for [`CheckpointAnchor`].
#[derive(Clone, Debug)]
pub struct AnchorConfig {
    /// Sequence we are trying to establish an anchor for.
    pub target_seq: u32,
    /// Number of distinct trusted validators that must agree on the same
    /// hash before we accept it. Typically 80% of the trusted UNL size.
    pub quorum: usize,
}

/// Tracks incoming validations for a single target sequence and reports the
/// hash once a UNL-quorum of distinct validators agree.
pub struct CheckpointAnchor {
    config: AnchorConfig,
    /// hash → set of validator public keys that agreed on it.
    by_hash: HashMap<Hash256, Vec<PublicKey>>,
    /// First hash that crossed quorum (if any).
    resolved: Option<Hash256>,
}

impl CheckpointAnchor {
    /// Create a fresh anchor tracker for `target_seq`.
    pub fn new(config: AnchorConfig) -> Self {
        Self {
            config,
            by_hash: HashMap::new(),
            resolved: None,
        }
    }

    /// Sequence we are tracking.
    pub fn target_seq(&self) -> u32 {
        self.config.target_seq
    }

    /// Quorum threshold.
    pub fn quorum(&self) -> usize {
        self.config.quorum
    }

    /// Has the anchor been resolved?
    pub fn resolved_hash(&self) -> Option<Hash256> {
        self.resolved
    }

    /// Feed a validation. Returns `Some(hash)` the first time a hash crosses
    /// quorum. Subsequent calls return `Some(hash)` of the same hash if it
    /// is the resolved one, or `None` if a different (and now necessarily
    /// minority) hash is offered.
    ///
    /// `is_trusted` is the caller's UNL filter — only validations whose
    /// signing key is trusted should reach this method. If you want to
    /// pre-filter, do it before calling.
    pub fn add(&mut self, validation: &Validation) -> Option<Hash256> {
        if validation.ledger_seq != self.config.target_seq || !validation.full {
            return None;
        }
        if let Some(h) = self.resolved {
            // Already resolved: only return Some for the matching hash.
            return if validation.ledger_hash == h {
                Some(h)
            } else {
                None
            };
        }
        let pk = match PublicKey::from_slice(&validation.public_key) {
            Ok(pk) => pk,
            Err(_) => return None,
        };
        let entry = self.by_hash.entry(validation.ledger_hash).or_default();
        if entry.iter().any(|existing| existing == &pk) {
            return None;
        }
        entry.push(pk);
        if entry.len() >= self.config.quorum {
            self.resolved = Some(validation.ledger_hash);
            return Some(validation.ledger_hash);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_consensus::types::NodeId;

    fn val(node_byte: u8, seq: u32, hash: Hash256, pk_byte: u8) -> Validation {
        Validation {
            node_id: NodeId(Hash256::new([node_byte; 32])),
            // 33-byte ed25519-prefixed key, distinct per validator.
            public_key: {
                let mut k = vec![0xED; 33];
                k[1] = pk_byte;
                k
            },
            ledger_hash: hash,
            ledger_seq: seq,
            full: true,
            close_time: 0,
            sign_time: 0,
            signature: None,
            amendments: vec![],
        }
    }

    #[test]
    fn quorum_reached() {
        let mut anchor = CheckpointAnchor::new(AnchorConfig {
            target_seq: 100,
            quorum: 3,
        });
        let h = Hash256::new([0xAA; 32]);
        assert!(anchor.add(&val(1, 100, h, 1)).is_none());
        assert!(anchor.add(&val(2, 100, h, 2)).is_none());
        assert_eq!(anchor.add(&val(3, 100, h, 3)), Some(h));
        assert_eq!(anchor.resolved_hash(), Some(h));
    }

    #[test]
    fn duplicate_validator_does_not_count_twice() {
        let mut anchor = CheckpointAnchor::new(AnchorConfig {
            target_seq: 100,
            quorum: 2,
        });
        let h = Hash256::new([0xBB; 32]);
        assert!(anchor.add(&val(1, 100, h, 1)).is_none());
        // Same key, even from a different "node_id", does not double-count.
        assert!(anchor.add(&val(2, 100, h, 1)).is_none());
        // A second distinct key does cross quorum.
        assert_eq!(anchor.add(&val(3, 100, h, 2)), Some(h));
    }

    #[test]
    fn wrong_seq_ignored() {
        let mut anchor = CheckpointAnchor::new(AnchorConfig {
            target_seq: 100,
            quorum: 1,
        });
        assert!(
            anchor
                .add(&val(1, 99, Hash256::new([0xAA; 32]), 1))
                .is_none()
        );
        assert_eq!(anchor.resolved_hash(), None);
    }

    #[test]
    fn non_full_validation_ignored() {
        let mut anchor = CheckpointAnchor::new(AnchorConfig {
            target_seq: 100,
            quorum: 1,
        });
        let mut v = val(1, 100, Hash256::new([0xCC; 32]), 1);
        v.full = false;
        assert!(anchor.add(&v).is_none());
    }

    #[test]
    fn split_brain_does_not_resolve() {
        let mut anchor = CheckpointAnchor::new(AnchorConfig {
            target_seq: 100,
            quorum: 3,
        });
        let h_a = Hash256::new([0xAA; 32]);
        let h_b = Hash256::new([0xBB; 32]);
        // Two validators on each fork — neither crosses quorum=3.
        assert!(anchor.add(&val(1, 100, h_a, 1)).is_none());
        assert!(anchor.add(&val(2, 100, h_a, 2)).is_none());
        assert!(anchor.add(&val(3, 100, h_b, 3)).is_none());
        assert!(anchor.add(&val(4, 100, h_b, 4)).is_none());
        assert!(anchor.resolved_hash().is_none());
    }
}
