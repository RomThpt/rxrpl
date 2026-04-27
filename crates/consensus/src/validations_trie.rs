//! Aggregates trusted validators' latest validations into a [`LedgerTrie`] for
//! preferred-branch discovery. Inspired by rippled `Validations<Adaptor>`
//! (xrpl/consensus/Validations.h) — but limited to the per-node "latest wins"
//! bookkeeping plus a trusted-set filter.
//!
//! Each trusted validator contributes at most one validation at a time. When a
//! newer validation arrives from a node that already had one, the previous
//! ledger hash is decremented out of the trie before the new one is inserted.
//! Untrusted validators are ignored entirely, mirroring rippled's behaviour
//! that only `trusted_set` validations enter the preferred-branch calculation.
//!
//! NIGHT-SHIFT-REVIEW: rippled keys ledgers by `LedgerID` and walks per-node
//! ancestry through the `Adaptor::acquire`-supplied chain. This port takes the
//! minimal route: each branch passed to [`LedgerTrie`] is a single-element
//! slice `[ledger_hash]`, i.e. the tip with no parent path. That suffices for
//! latest-wins aggregation and for `get_preferred` to return the branch with
//! the most validators voting for it. Chaining via `parent_ledger` (so common
//! ancestors share trie nodes) is a follow-up.
//!
//! NIGHT-SHIFT-REVIEW: `get_preferred(current_seq)` currently ignores
//! `current_seq` and delegates straight to [`LedgerTrie::get_preferred`].
//! Rippled uses the parameter to drop validations whose `ledger_seq` is older
//! than the caller's anchor sequence (so a stale validator can't pin
//! consensus to an obsolete ledger). Wire that filter in once the trie holds
//! per-validation sequences alongside hashes.

use std::collections::{HashMap, HashSet};

use rxrpl_primitives::Hash256;

use crate::ledger_trie::LedgerTrie;
use crate::types::{NodeId, Validation};

/// Aggregator that maps `(NodeId -> latest Validation)` and feeds each
/// trusted validator's latest hash into a [`LedgerTrie`] so that
/// [`get_preferred`](Self::get_preferred) returns the branch tip with the
/// most cumulative validator support.
pub struct ValidationsTrie {
    trie: LedgerTrie,
    /// Latest validation per node, keyed by `NodeId`. Only populated for
    /// trusted nodes — untrusted validations never enter the map.
    latest: HashMap<NodeId, Validation>,
    /// Trusted validator set. Validations from nodes outside this set are
    /// dropped on `add` and never contribute to the trie.
    trusted: HashSet<NodeId>,
}

impl Default for ValidationsTrie {
    fn default() -> Self {
        Self::new()
    }
}

impl ValidationsTrie {
    /// Create an empty aggregator with no trusted validators.
    pub fn new() -> Self {
        Self {
            trie: LedgerTrie::new(),
            latest: HashMap::new(),
            trusted: HashSet::new(),
        }
    }

    /// Mark `node` as trusted. Subsequent [`add`](Self::add) calls from this
    /// node will be tracked. Idempotent.
    pub fn add_trusted(&mut self, node: NodeId) {
        self.trusted.insert(node);
    }

    /// Remove `node` from the trusted set. Any validation it had previously
    /// contributed is decremented out of the trie and dropped from
    /// `latest`, so the preferred-branch calculation reflects the new UNL
    /// composition immediately.
    pub fn remove_trusted(&mut self, node: &NodeId) {
        self.trusted.remove(node);
        if let Some(prev) = self.latest.remove(node) {
            self.trie.remove(&[prev.ledger_hash], 1);
        }
    }

    /// Insert or replace `validation`'s contribution.
    ///
    /// Returns `true` when the call materially changed the trie state (a new
    /// trusted validator vote, or a trusted validator switched ledgers).
    /// Returns `false` when the validator is untrusted, or when the
    /// validation is identical to the latest one already on file (idempotent
    /// re-delivery).
    pub fn add(&mut self, validation: Validation) -> bool {
        let node_id = validation.node_id;
        if !self.trusted.contains(&node_id) {
            return false;
        }
        if let Some(prev) = self.latest.get(&node_id) {
            if prev.ledger_hash == validation.ledger_hash {
                // Same vote already counted — no-op.
                return false;
            }
            // Reject older validations (audit pass 2 C1): a stale validation
            // would otherwise overwrite the node's current vote in the trie.
            if validation.ledger_seq < prev.ledger_seq {
                return false;
            }
            if validation.ledger_seq == prev.ledger_seq && validation.sign_time <= prev.sign_time {
                return false;
            }
            // Switched ledger: pull old support out before crediting new.
            self.trie.remove(&[prev.ledger_hash], 1);
        }
        self.trie.insert(&[validation.ledger_hash], 1);
        self.latest.insert(node_id, validation);
        true
    }

    /// Return the preferred ledger hash given an anchor sequence.
    ///
    /// `current_seq` is accepted for API parity with rippled's
    /// `getPreferred(largestIssued)`; this minimal port delegates straight
    /// to [`LedgerTrie::get_preferred`] (see module-level review note).
    pub fn get_preferred(&self, _current_seq: u32) -> Option<Hash256> {
        self.trie.get_preferred()
    }

    /// Tip support for `hash` — the count of trusted validators whose latest
    /// validation is for this exact ledger.
    pub fn count_for(&self, hash: &Hash256) -> u32 {
        self.trie.tip_support(hash)
    }

    /// Number of validators currently in the trusted set.
    pub fn trusted_count(&self) -> usize {
        self.trusted.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeId {
        NodeId(Hash256::new([byte; 32]))
    }

    fn hash(byte: u8) -> Hash256 {
        let mut bytes = [0u8; 32];
        bytes[0] = byte;
        Hash256::new(bytes)
    }

    fn validation(node_byte: u8, hash_byte: u8, seq: u32, sign_time: u32) -> Validation {
        Validation {
            node_id: node(node_byte),
            public_key: vec![],
            ledger_hash: hash(hash_byte),
            ledger_seq: seq,
            full: true,
            close_time: sign_time,
            sign_time,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        }
    }

    #[test]
    fn empty_aggregator_returns_none() {
        let agg = ValidationsTrie::new();
        assert_eq!(agg.get_preferred(0), None);
        assert_eq!(agg.trusted_count(), 0);
        assert_eq!(agg.count_for(&hash(1)), 0);
    }

    #[test]
    fn one_trusted_validation_makes_its_hash_preferred() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add(validation(1, 0xAA, 5, 100)));
        assert_eq!(agg.get_preferred(5), Some(hash(0xAA)));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
    }

    #[test]
    fn two_trusted_validations_on_same_hash_increase_tip_support() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));

        assert!(agg.add(validation(1, 0xAA, 5, 100)));
        assert!(agg.add(validation(2, 0xAA, 5, 101)));

        assert_eq!(agg.count_for(&hash(0xAA)), 2);
        assert_eq!(agg.get_preferred(5), Some(hash(0xAA)));
    }

    #[test]
    fn same_node_different_hash_replaces_old_vote() {
        // Conflicting validations from the same node: latest wins, old is
        // decremented out of the trie. We seed two trusted validators on
        // hash 0xAA, then have node 1 switch to 0xBB. Only node 2 still
        // votes for 0xAA, so node 1's new vote on 0xBB makes the two tips
        // tied at one validator each — the lower-hash tie-break in
        // LedgerTrie picks 0xAA, but the count for 0xBB must be exactly 1
        // (proving node 1's old 0xAA vote was removed, not duplicated).
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));
        agg.add(validation(1, 0xAA, 5, 100));
        agg.add(validation(2, 0xAA, 5, 101));
        assert_eq!(agg.count_for(&hash(0xAA)), 2);

        assert!(agg.add(validation(1, 0xBB, 6, 110)));

        assert_eq!(agg.count_for(&hash(0xAA)), 1, "node 1's old vote not removed");
        assert_eq!(agg.count_for(&hash(0xBB)), 1, "node 1's new vote not added");
    }

    #[test]
    fn untrusted_validation_is_ignored() {
        let mut agg = ValidationsTrie::new();
        // node(99) is NOT in the trusted set.
        let changed = agg.add(validation(99, 0xCC, 5, 100));

        assert!(!changed, "add must return false for untrusted node");
        assert_eq!(agg.count_for(&hash(0xCC)), 0);
        assert_eq!(agg.get_preferred(5), None);
    }

    #[test]
    fn promoting_node_to_trusted_lets_their_validation_count() {
        // Validation from an untrusted node is dropped...
        let mut agg = ValidationsTrie::new();
        assert!(!agg.add(validation(7, 0xDD, 5, 100)));
        assert_eq!(agg.count_for(&hash(0xDD)), 0);

        // ...then we add the node to the trusted set and re-deliver the
        // same validation. Now it must count.
        agg.add_trusted(node(7));
        assert!(agg.add(validation(7, 0xDD, 5, 100)));
        assert_eq!(agg.count_for(&hash(0xDD)), 1);
        assert_eq!(agg.get_preferred(5), Some(hash(0xDD)));
    }

    #[test]
    fn idempotent_redelivery_returns_false_and_does_not_double_count() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add(validation(1, 0xEE, 5, 100)));
        // Re-delivering the exact same hash for the same node must not
        // increment tip support.
        assert!(!agg.add(validation(1, 0xEE, 5, 200)));
        assert_eq!(agg.count_for(&hash(0xEE)), 1);
    }

    #[test]
    fn remove_trusted_decrements_their_contribution() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));
        agg.add(validation(1, 0xAA, 5, 100));
        agg.add(validation(2, 0xAA, 5, 101));
        assert_eq!(agg.count_for(&hash(0xAA)), 2);

        agg.remove_trusted(&node(1));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
        assert_eq!(agg.trusted_count(), 1);

        // A subsequent add from the now-untrusted node must be ignored.
        assert!(!agg.add(validation(1, 0xAA, 6, 200)));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
    }

    #[test]
    fn replay_older_ledger_seq_rejected() {
        // Audit pass 2 C1: a validation with a strictly older ledger_seq
        // must NOT overwrite the node's current vote. Otherwise an
        // attacker can replay a stale validation and flip the trie's
        // preferred-branch vote.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add(validation(1, 0xAA, 10, 100)));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);

        // Replay an older seq for the same node — must be rejected.
        let stale = validation(1, 0xBB, 5, 200);
        assert!(!agg.add(stale), "older ledger_seq must be rejected");
        assert_eq!(agg.count_for(&hash(0xAA)), 1, "current vote unchanged");
        assert_eq!(agg.count_for(&hash(0xBB)), 0, "stale vote not counted");
        assert_eq!(agg.get_preferred(10), Some(hash(0xAA)));
    }

    #[test]
    fn replay_same_seq_older_sign_time_rejected() {
        // Audit pass 2 C1: at the same ledger_seq, a validation whose
        // sign_time is <= the current vote's sign_time must be rejected.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add(validation(1, 0xAA, 5, 100)));
        // Older sign_time at same seq — rejected.
        assert!(!agg.add(validation(1, 0xBB, 5, 50)));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
        assert_eq!(agg.count_for(&hash(0xBB)), 0);

        // Equal sign_time at same seq (different hash) — also rejected,
        // since strict monotonicity is required.
        assert!(!agg.add(validation(1, 0xCC, 5, 100)));
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
        assert_eq!(agg.count_for(&hash(0xCC)), 0);
    }

    #[test]
    fn replay_newer_sign_time_at_same_seq_accepted() {
        // Audit pass 2 C1: a validation at the same ledger_seq with a
        // strictly newer sign_time IS the legitimate "vote update" path
        // and must be accepted.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add(validation(1, 0xAA, 5, 100)));
        assert!(agg.add(validation(1, 0xBB, 5, 150)));
        assert_eq!(agg.count_for(&hash(0xAA)), 0, "old vote removed");
        assert_eq!(agg.count_for(&hash(0xBB)), 1, "new vote counted");
    }

    #[test]
    fn fork_majority_wins_preferred() {
        // Three trusted validators: 2 vote for hash 0x10, 1 votes for 0x20.
        // Branch with more support wins regardless of hash ordering.
        let mut agg = ValidationsTrie::new();
        for n in 1u8..=3 {
            agg.add_trusted(node(n));
        }
        agg.add(validation(1, 0x10, 5, 100));
        agg.add(validation(2, 0x10, 5, 101));
        agg.add(validation(3, 0x20, 5, 102));

        assert_eq!(agg.count_for(&hash(0x10)), 2);
        assert_eq!(agg.count_for(&hash(0x20)), 1);
        assert_eq!(agg.get_preferred(5), Some(hash(0x10)));
    }
}
