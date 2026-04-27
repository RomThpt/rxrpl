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

use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::types::{NodeId, Proposal};

/// Tracks the latest accepted proposal for each `(NodeId, prev_ledger)`.
#[derive(Debug, Default)]
pub struct ProposalTracker {
    proposals: HashMap<(NodeId, Hash256), Proposal>,
}

impl ProposalTracker {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self {
            proposals: HashMap::new(),
        }
    }

    /// Insert or update the proposal for its `(node_id, prev_ledger)` key.
    ///
    /// Returns `true` if accepted (new entry, or strictly newer `prop_seq`),
    /// `false` if the proposal is older than or duplicates the stored one.
    pub fn track(&mut self, proposal: Proposal) -> bool {
        let key = (proposal.node_id, proposal.prev_ledger);
        match self.proposals.get(&key) {
            Some(existing) if existing.prop_seq >= proposal.prop_seq => false,
            _ => {
                self.proposals.insert(key, proposal);
                true
            }
        }
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
    }

    /// Total number of `(node, prev_ledger)` entries currently tracked.
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// Whether the tracker has no entries.
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
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
        assert_eq!(
            tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq,
            5
        );
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
        assert_eq!(
            tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq,
            5
        );
    }

    #[test]
    fn same_node_different_prev_ledger_is_independent() {
        let mut tracker = ProposalTracker::new();
        // High prop_seq for ledger A.
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 9)));
        // Lower prop_seq but DIFFERENT prev_ledger: accepted as a new track.
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x20), 0)));
        assert_eq!(tracker.len(), 2);
        assert_eq!(
            tracker.get(&node(0x01), &ledger(0x10)).unwrap().prop_seq,
            9
        );
        assert_eq!(
            tracker.get(&node(0x01), &ledger(0x20)).unwrap().prop_seq,
            0
        );
    }

    #[test]
    fn different_nodes_share_prev_ledger() {
        let mut tracker = ProposalTracker::new();
        assert!(tracker.track(make_proposal(node(0x01), ledger(0x10), 0)));
        assert!(tracker.track(make_proposal(node(0x02), ledger(0x10), 0)));
        assert!(tracker.track(make_proposal(node(0x03), ledger(0x10), 2)));
        assert_eq!(tracker.count_for(&ledger(0x10)), 3);
        let seqs: Vec<u32> = {
            let mut v: Vec<u32> = tracker.iter_for(&ledger(0x10)).map(|p| p.prop_seq).collect();
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
    }
}
