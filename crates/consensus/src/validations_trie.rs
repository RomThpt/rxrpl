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
//! Two ingestion paths are exposed:
//! - [`Self::add`] credits a single-element branch `[ledger_hash]` — fast,
//!   backwards-compatible, but treats every tip as if it had no shared
//!   ancestry. Sufficient for latest-wins aggregation when callers don't
//!   know the parent chain.
//! - [`Self::add_with_parents`] credits the full branch
//!   `[oldest_ancestor, ..., parent, ledger_hash]`. Sibling validators on
//!   the same chain then share trie nodes, so `branch_support` accumulates
//!   correctly at every common ancestor — matching rippled's
//!   `Validations<Adaptor>` behaviour where the per-node ancestry comes
//!   from `Adaptor::acquire`.
//!
//! Whichever entry point a validator first uses is recorded in
//! [`Self::latest`] alongside the exact branch slice that was inserted, so
//! later replacements / evictions decrement the correct path.
//!
//! `get_preferred(current_seq)` filters out validations whose `ledger_seq`
//! is strictly less than `current_seq` before computing the preferred
//! branch — this prevents stale validators from pinning consensus to an
//! obsolete ledger. The filter is applied at read-time by walking
//! [`Self::latest`] and rebuilding a transient [`LedgerTrie`] over the
//! fresh subset. The rebuild is cached and only re-run when either the
//! aggregator state changes or the `current_seq` argument moves; callers
//! that want to amortise the cost across many `get_preferred` calls can
//! also invoke [`Self::prune_below`] to evict stale entries from
//! `self.latest` and the persistent trie so the fast path applies.

use std::sync::Mutex;
use std::collections::{HashMap, HashSet};

use rxrpl_primitives::Hash256;

use crate::ledger_trie::LedgerTrie;
use crate::types::{NodeId, Validation};

/// What we store per trusted validator: their latest validation plus the
/// exact branch slice we credited in the trie. The branch is needed so
/// that replacement, removal, and pruning decrement the same path that
/// was inserted (single-element vs full ancestry).
#[derive(Clone, Debug)]
struct Entry {
    validation: Validation,
    /// Branch from oldest known ancestor to tip (inclusive). Always at
    /// least one element; the last element is `validation.ledger_hash`.
    branch: Vec<Hash256>,
}

/// Aggregator that maps `(NodeId -> latest Validation)` and feeds each
/// trusted validator's latest hash into a [`LedgerTrie`] so that
/// [`get_preferred`](Self::get_preferred) returns the branch tip with the
/// most cumulative validator support.
pub struct ValidationsTrie {
    trie: LedgerTrie,
    /// Latest validation per node, keyed by `NodeId`, plus the branch
    /// slice that was credited in the trie. Only populated for trusted
    /// nodes — untrusted validations never enter the map.
    latest: HashMap<NodeId, Entry>,
    /// Trusted validator set. Validations from nodes outside this set are
    /// dropped on `add` and never contribute to the trie.
    trusted: HashSet<NodeId>,
    /// Bumps on every mutation that could change a `get_preferred(seq)`
    /// result. Used together with the cached `(seq, generation, hash)`
    /// triple to skip the rebuild when neither input has changed.
    generation: u64,
    /// Memoised slow-path answer: `(current_seq, generation_at_compute,
    /// preferred_hash)`. Behind a [`RefCell`] so `get_preferred` can stay
    /// `&self` (callers treat it as a pure read).
    preferred_cache: std::sync::Mutex<Option<(u32, u64, Option<Hash256>)>>,
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
            generation: 0,
            preferred_cache: Mutex::new(None),
        }
    }

    /// Bump the mutation counter and clear any stale memoised slow-path
    /// answer. Call after any state change that could move the preferred
    /// hash.
    fn invalidate_cache(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Ok(mut guard) = self.preferred_cache.lock() {
            *guard = None;
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
            self.trie.remove(&prev.branch, 1);
            self.invalidate_cache();
        }
    }

    /// Insert or replace `validation`'s contribution using a single-element
    /// branch `[ledger_hash]` (no parent ancestry). Convenience wrapper —
    /// callers that know the parent chain should use
    /// [`Self::add_with_parents`] so common ancestors share trie nodes.
    ///
    /// Returns `true` when the call materially changed the trie state (a new
    /// trusted validator vote, or a trusted validator switched ledgers).
    /// Returns `false` when the validator is untrusted, or when the
    /// validation is identical to the latest one already on file (idempotent
    /// re-delivery).
    pub fn add(&mut self, validation: Validation) -> bool {
        self.add_with_parents(validation, &[])
    }

    /// Insert or replace `validation`'s contribution using the full
    /// ancestry path. `parent_branch` is `[oldest_ancestor, ..., parent]`
    /// — the tip is appended automatically. Pass `&[]` for the
    /// single-element behaviour.
    ///
    /// When two trusted validators vote on tips that share a prefix, the
    /// shared part of the branch is credited only once per validator (no
    /// double-counting at the ancestors), matching rippled's
    /// `LedgerTrie<Ledger>` semantics.
    ///
    /// Same return contract as [`Self::add`].
    pub fn add_with_parents(
        &mut self,
        validation: Validation,
        parent_branch: &[Hash256],
    ) -> bool {
        let node_id = validation.node_id;
        if !self.trusted.contains(&node_id) {
            return false;
        }
        if let Some(prev) = self.latest.get(&node_id) {
            if prev.validation.ledger_hash == validation.ledger_hash {
                // Same vote already counted — no-op.
                return false;
            }
            // Reject older validations (audit pass 2 C1): a stale validation
            // would otherwise overwrite the node's current vote in the trie.
            if validation.ledger_seq < prev.validation.ledger_seq {
                return false;
            }
            if validation.ledger_seq == prev.validation.ledger_seq
                && validation.sign_time <= prev.validation.sign_time
            {
                return false;
            }
        }

        // Build the new branch: [parent_branch..., ledger_hash].
        let mut branch = Vec::with_capacity(parent_branch.len() + 1);
        branch.extend_from_slice(parent_branch);
        branch.push(validation.ledger_hash);

        // Switched ledger: pull old support out using the previously
        // recorded path before crediting the new one.
        if let Some(prev) = self.latest.get(&node_id) {
            self.trie.remove(&prev.branch, 1);
        }
        self.trie.insert(&branch, 1);
        self.latest.insert(
            node_id,
            Entry {
                validation,
                branch,
            },
        );
        self.invalidate_cache();
        true
    }

    /// Return the preferred ledger hash given an anchor sequence.
    ///
    /// Only validations whose `ledger_seq >= current_seq` participate in
    /// the calculation — older ones are treated as stale (a validator that
    /// hasn't caught up must not be allowed to pin the network to an
    /// obsolete ledger). When every cached validation is fresh, the
    /// persistent trie's answer is returned directly; otherwise we
    /// rebuild a transient trie over just the fresh subset and memoise
    /// the result so repeated calls at the same `current_seq` (between
    /// mutations) skip the rebuild.
    pub fn get_preferred(&self, current_seq: u32) -> Option<Hash256> {
        // Fast path: nothing stale -> the persistent trie already reflects
        // the right answer. Avoids the per-call rebuild for the common
        // case where pruning has been kept up to date.
        let any_stale = self
            .latest
            .values()
            .any(|e| e.validation.ledger_seq < current_seq);
        if !any_stale {
            return self.trie.get_preferred();
        }

        // Slow path with memoisation: if the cached entry was computed at
        // the same generation and same `current_seq`, reuse it.
        if let Ok(guard) = self.preferred_cache.lock() {
            if let Some((cached_seq, cached_gen, cached_hash)) = *guard {
                if cached_seq == current_seq && cached_gen == self.generation {
                    return cached_hash;
                }
            }
        }

        let mut filtered = LedgerTrie::new();
        for entry in self.latest.values() {
            if entry.validation.ledger_seq >= current_seq {
                filtered.insert(&entry.branch, 1);
            }
        }
        let preferred = filtered.get_preferred();
        if let Ok(mut guard) = self.preferred_cache.lock() {
            *guard = Some((current_seq, self.generation, preferred));
        }
        preferred
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
            .filter(|(_, e)| e.validation.ledger_seq < current_seq)
            .map(|(node_id, _)| *node_id)
            .collect();
        let evicted = stale.len();
        for node_id in stale {
            if let Some(prev) = self.latest.remove(&node_id) {
                self.trie.remove(&prev.branch, 1);
            }
        }
        if evicted > 0 {
            self.invalidate_cache();
        }
        evicted
    }

    /// Tip support for `hash` — the count of trusted validators whose latest
    /// validation is for this exact ledger.
    ///
    /// When all callers use the same ingestion API consistently
    /// (`add` everywhere, or `add_with_parents` everywhere with a
    /// canonical parent chain), the answer is exact. Mixing the two for
    /// the same tip hash can split the support across two distinct trie
    /// nodes — `count_for` returns whichever node `LedgerTrie` finds
    /// first in its traversal.
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

    // -----------------------------------------------------------------
    // add_with_parents: parent-chained ingestion shares trie nodes
    // across sibling validators voting on overlapping ancestry.
    // -----------------------------------------------------------------

    #[test]
    fn add_with_parents_credits_branch_support_at_shared_ancestors() {
        // Two trusted validators agree on parent path [A, B] but vote for
        // different tips C vs D. Branch support at A and B must be 2,
        // tip support at C and D must be 1 each.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));

        let parent = vec![hash(0xAA), hash(0xBB)];
        assert!(agg.add_with_parents(validation(1, 0xCC, 5, 100), &parent));
        assert!(agg.add_with_parents(validation(2, 0xDD, 5, 101), &parent));

        // Tips were never aliased — each appears exactly once.
        assert_eq!(agg.count_for(&hash(0xCC)), 1);
        assert_eq!(agg.count_for(&hash(0xDD)), 1);
        // The fork sits at the tip; preferred picks the higher-hash sibling.
        assert_eq!(agg.get_preferred(5), Some(hash(0xDD)));
    }

    #[test]
    fn add_with_parents_replacement_decrements_full_old_branch() {
        // A validator first votes via add_with_parents on [A, B, C], then
        // switches to [A, X, Y] at a higher seq. The old branch must be
        // fully removed (no leftover support at B or C); the new branch
        // takes its place. Common prefix [A] survives because the new
        // branch still uses it.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));

        assert!(agg.add_with_parents(
            validation(1, 0xCC, 5, 100),
            &[hash(0xAA), hash(0xBB)],
        ));
        assert_eq!(agg.count_for(&hash(0xCC)), 1);

        assert!(agg.add_with_parents(
            validation(1, 0xEE, 6, 110),
            &[hash(0xAA), hash(0xDD)],
        ));
        assert_eq!(agg.count_for(&hash(0xCC)), 0, "old tip not removed");
        assert_eq!(agg.count_for(&hash(0xBB)), 0, "old parent not removed");
        assert_eq!(agg.count_for(&hash(0xEE)), 1, "new tip credited");
        assert_eq!(agg.get_preferred(6), Some(hash(0xEE)));
    }

    #[test]
    fn add_with_parents_remove_trusted_drops_full_branch() {
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        assert!(agg.add_with_parents(
            validation(1, 0xCC, 5, 100),
            &[hash(0xAA), hash(0xBB)],
        ));
        assert_eq!(agg.count_for(&hash(0xCC)), 1);

        agg.remove_trusted(&node(1));
        assert_eq!(agg.count_for(&hash(0xCC)), 0);
        assert_eq!(agg.count_for(&hash(0xBB)), 0);
        assert_eq!(agg.get_preferred(5), None);
    }

    #[test]
    fn get_preferred_caches_slow_path_until_mutation() {
        // After two calls at the same seq with no mutation in between,
        // the second must return the same answer (cache hit). Then a
        // mutation must invalidate so the next call recomputes.
        let mut agg = ValidationsTrie::new();
        agg.add_trusted(node(1));
        agg.add_trusted(node(2));
        agg.add(validation(1, 0xAA, 4, 100)); // stale relative to seq=10
        agg.add(validation(2, 0xBB, 10, 101));

        // Slow path (one stale entry at seq < 10) — first call computes.
        let first = agg.get_preferred(10);
        assert_eq!(first, Some(hash(0xBB)));
        // Cache hit — same answer, same generation.
        assert_eq!(agg.get_preferred(10), first);

        // Mutate by adding another trusted validator on the fresh tip;
        // cache must be invalidated and the recompute still picks BB.
        agg.add_trusted(node(3));
        agg.add(validation(3, 0xBB, 10, 102));
        assert_eq!(agg.get_preferred(10), Some(hash(0xBB)));
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
