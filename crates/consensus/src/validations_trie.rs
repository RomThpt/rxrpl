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
//! `get_preferred(current_seq)` filters out validations whose `ledger_seq`
//! is strictly less than `current_seq` before computing the preferred
//! branch — this prevents stale validators from pinning consensus to an
//! obsolete ledger. The filter is applied at read-time by walking
//! [`Self::latest`] and rebuilding a transient [`LedgerTrie`] over the
//! fresh subset; callers that want to amortise the cost across many
//! `get_preferred` calls can invoke [`Self::prune_below`] first to evict
//! stale entries from `self.latest` and the persistent trie.

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
            // Switched ledger: pull old support out before crediting new.
            self.trie.remove(&[prev.ledger_hash], 1);
        }
        self.trie.insert(&[validation.ledger_hash], 1);
        self.latest.insert(node_id, validation);
        true
    }

    /// Return the preferred ledger hash given an anchor sequence.
    ///
    /// Only validations whose `ledger_seq >= current_seq` participate in
    /// the calculation — older ones are treated as stale (a validator that
    /// hasn't caught up must not be allowed to pin the network to an
    /// obsolete ledger). When every cached validation is fresh, the
    /// persistent trie's answer is returned directly; otherwise we
    /// rebuild a transient trie over just the fresh subset.
    pub fn get_preferred(&self, current_seq: u32) -> Option<Hash256> {
        // Fast path: nothing stale -> the persistent trie already reflects
        // the right answer. Avoids the per-call rebuild for the common
        // case where pruning has been kept up to date.
        let any_stale = self
            .latest
            .values()
            .any(|v| v.ledger_seq < current_seq);
        if !any_stale {
            return self.trie.get_preferred();
        }

        let mut filtered = LedgerTrie::new();
        for validation in self.latest.values() {
            if validation.ledger_seq >= current_seq {
                filtered.insert(&[validation.ledger_hash], 1);
            }
        }
        filtered.get_preferred()
    }

    /// Drop every cached validation whose `ledger_seq < current_seq` and
    /// remove its support from the persistent trie. Returns the number of
    /// entries evicted. Idempotent: calling twice with the same threshold
    /// is a no-op the second time.
    ///
    /// Callers that drive consensus over many sequences should invoke this
    /// whenever the anchor advances so [`Self::get_preferred`] can take
    /// the fast path.
    pub fn prune_below(&mut self, current_seq: u32) -> usize {
        let stale: Vec<NodeId> = self
            .latest
            .iter()
            .filter(|(_, v)| v.ledger_seq < current_seq)
            .map(|(node_id, _)| *node_id)
            .collect();
        let evicted = stale.len();
        for node_id in stale {
            if let Some(prev) = self.latest.remove(&node_id) {
                self.trie.remove(&[prev.ledger_hash], 1);
            }
        }
        evicted
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
        // tied at one validator each — the higher-hash tie-break in
        // LedgerTrie picks 0xBB, but the count for 0xBB must be exactly 1
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

    // -----------------------------------------------------------------
    // H5: get_preferred(current_seq) MUST filter validations whose
    // ledger_seq < current_seq so a stale validator can't pin consensus
    // to an obsolete ledger.
    // -----------------------------------------------------------------

    #[test]
    fn get_preferred_drops_validations_below_current_seq() {
        // Two stale validators voting for hash 0xAA at seq=4, one fresh
        // validator voting for hash 0xBB at seq=10. Anchor seq=10 means
        // only the fresh validator counts: preferred = 0xBB.
        let mut agg = ValidationsTrie::new();
        for n in 1u8..=3 {
            agg.add_trusted(node(n));
        }
        agg.add(validation(1, 0xAA, 4, 100));
        agg.add(validation(2, 0xAA, 4, 101));
        agg.add(validation(3, 0xBB, 10, 102));

        // Without the seq filter, 0xAA would win 2-1. The filter must drop
        // the two stale votes, leaving 0xBB as the only counted hash.
        assert_eq!(agg.get_preferred(10), Some(hash(0xBB)));
        // A lower anchor seq counts everyone -> 0xAA wins by support.
        assert_eq!(agg.get_preferred(4), Some(hash(0xAA)));
    }

    #[test]
    fn get_preferred_returns_none_when_all_validations_are_stale() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));
        agg.add(validation(1, 0xAA, 3, 100));
        agg.add(validation(2, 0xBB, 4, 101));

        // Anchor at seq=10: every cached validation is older.
        assert_eq!(agg.get_preferred(10), None);
    }

    #[test]
    fn get_preferred_keeps_validations_at_current_seq() {
        // A validation exactly at the anchor sequence is fresh, not stale.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add(validation(1, 0xCC, 7, 100));

        assert_eq!(agg.get_preferred(7), Some(hash(0xCC)));
        assert_eq!(agg.get_preferred(8), None);
    }

    #[test]
    fn prune_below_evicts_stale_entries_and_decrements_trie() {
        let mut agg = ValidationsTrie::new();
        for n in 1u8..=3 {
            agg.add_trusted(node(n));
        }
        agg.add(validation(1, 0xAA, 4, 100));
        agg.add(validation(2, 0xAA, 4, 101));
        agg.add(validation(3, 0xBB, 10, 102));

        let evicted = agg.prune_below(10);
        assert_eq!(evicted, 2);
        // Stale tip support must be removed from the persistent trie.
        assert_eq!(agg.count_for(&hash(0xAA)), 0);
        assert_eq!(agg.count_for(&hash(0xBB)), 1);
        // After pruning, get_preferred on the same threshold takes the fast
        // path and still returns the fresh hash.
        assert_eq!(agg.get_preferred(10), Some(hash(0xBB)));
    }

    #[test]
    fn prune_below_is_idempotent() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add(validation(1, 0xAA, 4, 100));

        assert_eq!(agg.prune_below(10), 1);
        // Second call: nothing left to evict.
        assert_eq!(agg.prune_below(10), 0);
        assert_eq!(agg.count_for(&hash(0xAA)), 0);
    }

    #[test]
    fn prune_below_keeps_fresh_entries_intact() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));
        agg.add(validation(1, 0xAA, 10, 100));
        agg.add(validation(2, 0xBB, 12, 101));

        // Threshold sits below every cached seq -> nothing evicted.
        assert_eq!(agg.prune_below(5), 0);
        assert_eq!(agg.count_for(&hash(0xAA)), 1);
        assert_eq!(agg.count_for(&hash(0xBB)), 1);
    }
}
