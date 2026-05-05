//! Tracks the latest peer proposal per (NodeId, prev_ledger). Inspired by goXRPL
//! `internal/consensus/rcl/proposals.go::ProposalTracker`.
//!
//! Acceptance rule (mirrors goXRPL `Add`):
//! - Insert if no existing proposal exists for `(node_id, prev_ledger)`.
//! - Replace if the existing proposal's `prop_seq` is strictly less than the new one.
//! - Otherwise reject (older or duplicate).
//!
//! The (NodeId, prev_ledger) pair acts as the round key: a peer building on a
//! different prev_ledger is treated as an independent track, so out-of-order
//! `prop_seq` values from a different round do not poison the new round.
//!
//! ## DoS hardening (audit pass 2 H6)
//!
//! The tracker enforces two capacity bounds to prevent unbounded memory growth
//! from peers (trusted or untrusted, since [`track`] is called before any UNL
//! gate by callers like the simulator and tests):
//!
//! - [`MAX_DISTINCT_PREV_LEDGERS`]: total number of distinct `prev_ledger`
//!   hashes the tracker will retain. When a *new* prev_ledger arrives and the
//!   cap is reached, the oldest prev_ledger (by insertion order) is evicted
//!   along with all proposals keyed on it. This is a coarse FIFO/LRU.
//! - [`MAX_PROPOSALS_PER_PREV`]: per-prev_ledger node count. Updates to an
//!   existing `(node_id, prev_ledger)` entry always proceed (subject to the
//!   `prop_seq` rule); only *new* node insertions for an already-saturated
//!   prev_ledger are rejected. The cap is sized well above a typical UNL so
//!   honest rounds are unaffected.
//!
//! [`track`]: ProposalTracker::track

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use rxrpl_primitives::Hash256;

use crate::types::{NodeId, Proposal};

/// Maximum number of distinct `prev_ledger` hashes the tracker will retain.
///
/// When a proposal arrives with a `prev_ledger` not yet tracked AND this cap
/// is already reached, the oldest tracked `prev_ledger` (by insertion order)
/// and every proposal keyed on it are evicted to make room. Sized to comfortably
/// cover normal in-flight rounds (typically 1-2 prev_ledgers concurrently)
/// while bounding memory under adversarial fan-out.
pub const MAX_DISTINCT_PREV_LEDGERS: usize = 64;

/// Maximum number of distinct nodes whose proposals are tracked per
/// `prev_ledger`.
///
/// Sized as "16 distinct nodes per round", which exceeds typical UNL sizes
/// for the keys actually relevant to a single round; updates to an
/// already-tracked `(node_id, prev_ledger)` entry are not rejected by this
/// cap.
pub const MAX_PROPOSALS_PER_PREV: usize = 16;

/// Tracks the latest accepted proposal for each `(NodeId, prev_ledger)`.
#[derive(Debug, Default)]
pub struct ProposalTracker {
    proposals: HashMap<(NodeId, Hash256), Proposal>,
    /// Insertion-order list of distinct `prev_ledger` hashes currently
    /// represented in `proposals`. Front = oldest, back = newest. Used to
    /// evict the oldest prev_ledger track when [`MAX_DISTINCT_PREV_LEDGERS`]
    /// is exceeded.
    prev_ledger_order: Vec<Hash256>,
    /// Counter: proposals rejected by [`Self::track`] because the existing
    /// entry's `prop_seq` is greater-than-or-equal to the incoming one
    /// (duplicate or stale-seq replay). Mirrors rippled's
    /// `Counter[ConsensusProposals.duplicateOrStaleSeq]` JLOG metric.
    /// Capacity-bound rejections (per-prev-ledger node cap) are NOT
    /// counted here; only the strict prop_seq monotonicity rule.
    /// Exposed via [`Self::proposals_dropped_dedup_total`].
    proposals_dropped_dedup_total: AtomicU64,
}

impl ProposalTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
            prev_ledger_order: Vec::new(),
            proposals_dropped_dedup_total: AtomicU64::new(0),
        }
    }

    /// Total number of proposals rejected by [`Self::track`] because their
    /// `prop_seq` was less than or equal to the already-tracked entry for
    /// the same `(node_id, prev_ledger)` key. Mirrors rippled's
    /// `Counter[ConsensusProposals.duplicateOrStaleSeq]` JLOG metric.
    /// Cap-driven evictions (per-prev-ledger node cap) are NOT included.
    pub fn proposals_dropped_dedup_total(&self) -> u64 {
        self.proposals_dropped_dedup_total.load(Ordering::Relaxed)
    }

    /// Insert or update the proposal for its `(node_id, prev_ledger)` key.
    ///
    /// Returns `true` if accepted (new entry, or strictly newer `prop_seq`),
    /// `false` if rejected because:
    /// - the proposal is older than or duplicates the stored one, OR
    /// - this is a new node entry for `prev_ledger` and the per-prev_ledger
    ///   cap [`MAX_PROPOSALS_PER_PREV`] is already saturated.
    ///
    /// Side-effect: if `proposal.prev_ledger` is not already tracked and the
    /// distinct-prev_ledger cap [`MAX_DISTINCT_PREV_LEDGERS`] would be
    /// exceeded, the oldest tracked prev_ledger is evicted along with all
    /// proposals keyed on it (FIFO).
    pub fn track(&mut self, proposal: Proposal) -> bool {
        let key = (proposal.node_id, proposal.prev_ledger);

        // Existing entry: standard prop_seq replacement rule, no caps apply.
        if let Some(existing) = self.proposals.get(&key) {
            if existing.prop_seq >= proposal.prop_seq {
                self.proposals_dropped_dedup_total
                    .fetch_add(1, Ordering::Relaxed);
                return false;
            }
            self.proposals.insert(key, proposal);
            return true;
        }

        // New entry. First check the per-prev_ledger node count cap.
        let prev_ledger = proposal.prev_ledger;
        let is_new_prev = !self.prev_ledger_order.contains(&prev_ledger);
        if !is_new_prev {
            let count = self.count_for(&prev_ledger);
            if count >= MAX_PROPOSALS_PER_PREV {
                return false;
            }
        } else if self.prev_ledger_order.len() >= MAX_DISTINCT_PREV_LEDGERS {
            // New prev_ledger and we're at the distinct-prev_ledger cap.
            // Evict the oldest tracked prev_ledger (FIFO) and every proposal
            // keyed on it.
            let oldest = self.prev_ledger_order.remove(0);
            self.proposals.retain(|(_, prev), _| prev != &oldest);
        }

        self.proposals.insert(key, proposal);
        if is_new_prev {
            self.prev_ledger_order.push(prev_ledger);
        }
        true
    }

    /// Return the stored proposal for `(node_id, prev_ledger)`, if any.
    pub fn get(&self, node_id: &NodeId, prev_ledger: &Hash256) -> Option<&Proposal> {
        self.proposals.get(&(*node_id, *prev_ledger))
    }

    /// Count how many distinct nodes have a proposal for `prev_ledger`.
    pub fn count_for(&self, prev_ledger: &Hash256) -> usize {
        self.proposals
            .keys()
            .filter(|(_, prev)| prev == prev_ledger)
            .count()
    }

    /// Iterate over all proposals tracked for `prev_ledger`.
    pub fn iter_for<'a>(
        &'a self,
        prev_ledger: &'a Hash256,
    ) -> impl Iterator<Item = &'a Proposal> + 'a {
        self.proposals
            .iter()
            .filter_map(move |((_, prev), p)| (prev == prev_ledger).then_some(p))
    }

    /// Drop all proposals tracked for `prev_ledger`.
    pub fn clear_for(&mut self, prev_ledger: &Hash256) {
        self.proposals.retain(|(_, prev), _| prev != prev_ledger);
        self.prev_ledger_order.retain(|h| h != prev_ledger);
    }

    /// Total number of `(node, prev_ledger)` entries currently tracked.
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// Whether the tracker has no entries.
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }

    /// Number of distinct `prev_ledger` hashes currently tracked. Exposed for
    /// tests and metrics around the [`MAX_DISTINCT_PREV_LEDGERS`] cap.
    pub fn distinct_prev_ledgers(&self) -> usize {
        self.prev_ledger_order.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(byte: u8) -> NodeId {
        NodeId(Hash256::new([byte; 32]))
    }

    fn ledger(byte: u8) -> Hash256 {
        Hash256::new([byte; 32])
    }

    fn ledger_from_index(i: usize) -> Hash256 {
        let mut b = [0u8; 32];
        b[0..8].copy_from_slice(&(i as u64).to_be_bytes());
        Hash256::new(b)
    }

    fn make_proposal(node_id: NodeId, prev_ledger: Hash256, prop_seq: u32) -> Proposal {
        Proposal {
            node_id,
            public_key: Vec::new(),
            tx_set_hash: Hash256::new([0xAA; 32]),
            close_time: 100,
            prop_seq,
            ledger_seq: 1,
            prev_ledger,
            signature: None,
        }
    }

    #[test]
    fn new_tracker_is_empty() {
        let tracker = ProposalTracker::new();
        assert_eq!(tracker.len(), 0);
        assert!(tracker.is_empty());
        assert_eq!(tracker.count_for(&ledger(0x01)), 0);
        assert!(tracker.get(&node(0x01), &ledger(0x01)).is_none());
    }

    #[test]
    fn first_proposal_is_accepted() {
        let mut tracker = ProposalTracker::new();
        let p = make_proposal(node(0x01), ledger(0x10), 0);
        assert!(tracker.track(p));
        assert_eq!(tracker.len(), 1);
        let stored = tracker.get(&node(0x01), &ledger(0x10)).unwrap();
        assert_eq!(stored.prop_seq, 0);
    }

    #[test]
    fn higher_prop_seq_replaces_existing() {
        let mut tracker = ProposalTracker::new();
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 0)));
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 1)));
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 5)));
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq, 5);
    }

    #[test]
    fn lower_or_equal_prop_seq_is_rejected() {
        let mut tracker = ProposalTracker::new();
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 5)));
        // Equal prop_seq: rejected as duplicate.
        assert!(!tracker.track(make_proposal(node(0x01), ledger(0x10), 5)));
        // Strictly lower: rejected as stale.
        assert!(!tracker.track(make_proposal(node(0x01), ledger(0x10), 3)));
        // Stored proposal must still be the original.
        assert_eq!(tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq, 5);
    }

    #[test]
    fn same_node_different_prev_ledger_is_independent() {
        let mut tracker = ProposalTracker::new();
        // High prop_seq for ledger A.
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 9)));
        // Lower prop_seq but DIFFERENT prev_ledger: accepted as a new track.
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x20), 0)));
        assert_eq!(tracker.len(), 2);
        assert_eq!(tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq, 9);
        assert_eq!(tracker.get(&node(0x01), &ledger(0x20)).unwrap().prop_seq, 0);
    }

    #[test]
    fn different_nodes_share_prev_ledger() {
        let mut tracker = ProposalTracker::new();
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 0)));
        assert!(tracker.track(make_proposal(node(0x02), ledger(0x10), 0)));
        assert!(tracker.track(make_proposal(node(0x03), ledger(0x10), 2)));
        assert_eq!(tracker.count_for(&ledger(0x10)), 3);
        let seqs: Vec<u32> = {
            let mut v: Vec<u32> = tracker
                .iter_for(&ledger(0x10))
                .map(|p| p.prop_seq)
                .collect();
            v.sort_unstable();
            v
        };
        assert_eq!(seqs, vec![0, 0, 2]);
    }

    #[test]
    fn clear_for_removes_only_target_prev_ledger() {
        let mut tracker = ProposalTracker::new();
        tracker.track(make_proposal(node(0x01), ledger(0x10), 0));
        tracker.track(make_proposal(node(0x02), ledger(0x10), 1));
        tracker.track(make_proposal(node(0x01), ledger(0x20), 0));
        assert_eq!(tracker.len(), 3);

        tracker.clear_for(&ledger(0x10));
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.count_for(&ledger(0x10)), 0);
        assert_eq!(tracker.count_for(&ledger(0x20)), 1);
        assert!(tracker.get(&node(0x01), &ledger(0x20)).is_some());
        assert_eq!(tracker.distinct_prev_ledgers(), 1);
    }

    // --- DoS hardening (audit pass 2 H6) ---

    #[test]
    fn distinct_prev_ledger_cap_evicts_oldest() {
        let mut tracker = ProposalTracker::new();

        // Insert MAX_DISTINCT_PREV_LEDGERS distinct prev_ledger keys, each
        // from a single node so len() == MAX_DISTINCT_PREV_LEDGERS.
        for i in 0..MAX_DISTINCT_PREV_LEDGERS {
            let prev = ledger_from_index(i);
            assert!(tracker.track(make_proposal(node(0x01), prev, 0)));
        }
        assert_eq!(tracker.distinct_prev_ledgers(), MAX_DISTINCT_PREV_LEDGERS);
        assert_eq!(tracker.len(), MAX_DISTINCT_PREV_LEDGERS);

        // Insert one more distinct prev_ledger: cap holds at
        // MAX_DISTINCT_PREV_LEDGERS, oldest (index 0) evicted.
        let new_prev = ledger_from_index(MAX_DISTINCT_PREV_LEDGERS);
        assert!(tracker.track(make_proposal(node(0x01), new_prev, 0)));
        assert_eq!(tracker.distinct_prev_ledgers(), MAX_DISTINCT_PREV_LEDGERS);
        assert_eq!(tracker.len(), MAX_DISTINCT_PREV_LEDGERS);

        // Oldest (index 0) was evicted along with its proposal.
        let evicted = ledger_from_index(0);
        assert!(tracker.get(&node(0x01), &evicted).is_none());
        assert_eq!(tracker.count_for(&evicted), 0);

        // The newly inserted prev_ledger is present.
        assert!(tracker.get(&node(0x01), &new_prev).is_some());

        // The next-oldest (index 1) survives.
        let survivor = ledger_from_index(1);
        assert!(tracker.get(&node(0x01), &survivor).is_some());
    }

    #[test]
    fn per_prev_ledger_cap_rejects_overflow_node() {
        let mut tracker = ProposalTracker::new();
        let prev = ledger(0x10);

        // Fill exactly to MAX_PROPOSALS_PER_PREV with distinct nodes.
        for i in 0..MAX_PROPOSALS_PER_PREV {
            let n = node((i + 1) as u8);
            assert!(tracker.track(make_proposal(n, prev, 0)));
        }
        assert_eq!(tracker.count_for(&prev), MAX_PROPOSALS_PER_PREV);

        // The (MAX+1)-th distinct node MUST be rejected.
        let overflow_node = node(0xFF);
        assert!(!tracker.track(make_proposal(overflow_node, prev, 0)));
        assert_eq!(tracker.count_for(&prev), MAX_PROPOSALS_PER_PREV);
        assert!(tracker.get(&overflow_node, &prev).is_none());

        // Updates to an *existing* tracked node are still accepted (cap only
        // gates new node entries).
        let existing = node(1);
        assert!(tracker.track(make_proposal(existing, prev, 99)));
        assert_eq!(tracker.count_for(&prev), MAX_PROPOSALS_PER_PREV);
        assert_eq!(tracker.get(&existing, &prev).unwrap().prop_seq, 99);
    }

    #[test]
    fn dedup_counter_bumps_on_prop_seq_reject_only() {
        // T34: proposals_dropped_dedup_total must increment on every
        // prop_seq monotonicity rejection and ONLY on those. Cap-driven
        // rejections (per-prev-ledger node cap) must NOT increment it.
        let mut tracker = ProposalTracker::new();
        let prev = ledger(0x10);

        // First insert is accepted, counter unchanged.
        assert!(tracker.track(make_proposal(node(0x01), prev, 5)));
        assert_eq!(tracker.proposals_dropped_dedup_total(), 0);

        // Equal prop_seq from same node: rejected as duplicate, counter +1.
        assert!(!tracker.track(make_proposal(node(0x01), prev, 5)));
        assert_eq!(tracker.proposals_dropped_dedup_total(), 1);

        // Strictly lower prop_seq: rejected as stale, counter +1.
        assert!(!tracker.track(make_proposal(node(0x01), prev, 3)));
        assert_eq!(tracker.proposals_dropped_dedup_total(), 2);

        // Strictly higher prop_seq: accepted, counter unchanged.
        assert!(tracker.track(make_proposal(node(0x01), prev, 6)));
        assert_eq!(tracker.proposals_dropped_dedup_total(), 2);

        // A different node sharing the same prev_ledger: a NEW entry is
        // inserted, no dedup happens.
        assert!(tracker.track(make_proposal(node(0x02), prev, 0)));
        assert_eq!(tracker.proposals_dropped_dedup_total(), 2);

        // Saturate the per-prev_ledger cap with fresh nodes, then attempt
        // an overflow node: this is rejected by the CAP rule, NOT the
        // dedup rule. Counter must remain unchanged.
        // Two slots already used (nodes 0x01 + 0x02); fill the rest.
        for i in 2..MAX_PROPOSALS_PER_PREV {
            let n = node((i + 1) as u8);
            assert!(tracker.track(make_proposal(n, prev, 0)));
        }
        assert_eq!(tracker.count_for(&prev), MAX_PROPOSALS_PER_PREV);

        let dedup_before_cap_drop = tracker.proposals_dropped_dedup_total();
        let overflow_node = node(0xFF);
        assert!(!tracker.track(make_proposal(overflow_node, prev, 0)));
        // Cap rejection MUST NOT bump the dedup counter.
        assert_eq!(
            tracker.proposals_dropped_dedup_total(),
            dedup_before_cap_drop
        );
    }

    #[test]
    fn per_prev_cap_is_independent_across_prev_ledgers() {
        // Saturating one prev_ledger does not affect another.
        let mut tracker = ProposalTracker::new();
        let prev_a = ledger(0xA0);
        let prev_b = ledger(0xB0);

        for i in 0..MAX_PROPOSALS_PER_PREV {
            assert!(tracker.track(make_proposal(node((i + 1) as u8), prev_a, 0)));
        }
        assert_eq!(tracker.count_for(&prev_a), MAX_PROPOSALS_PER_PREV);

        // First node on prev_b is accepted even though prev_a is saturated.
        assert!(tracker.track(make_proposal(node(0x01), prev_b, 0)));
        assert_eq!(tracker.count_for(&prev_b), 1);
    }
}
