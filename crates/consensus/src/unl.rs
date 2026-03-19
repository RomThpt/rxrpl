use std::collections::HashSet;

use crate::types::NodeId;

/// Trusted Validator List (UNL).
///
/// Determines which validators count toward consensus quorum.
/// An empty UNL means solo mode (all proposals accepted).
#[derive(Clone, Debug, Default)]
pub struct TrustedValidatorList {
    /// Validators we trust.
    trusted: HashSet<NodeId>,
    /// Validators temporarily removed from quorum (e.g., unreliable).
    negative_unl: HashSet<NodeId>,
}

impl TrustedValidatorList {
    /// Create a new UNL from a set of trusted node IDs.
    pub fn new(trusted: HashSet<NodeId>) -> Self {
        Self {
            trusted,
            negative_unl: HashSet::new(),
        }
    }

    /// Create an empty UNL (solo mode).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if a node is trusted (and not in the negative UNL).
    pub fn is_trusted(&self, node_id: &NodeId) -> bool {
        self.trusted.contains(node_id) && !self.negative_unl.contains(node_id)
    }

    /// Check if the UNL is empty (solo mode).
    pub fn is_empty(&self) -> bool {
        self.trusted.is_empty()
    }

    /// Add a node to the negative UNL.
    pub fn add_to_negative_unl(&mut self, node_id: NodeId) {
        self.negative_unl.insert(node_id);
    }

    /// Remove a node from the negative UNL.
    pub fn remove_from_negative_unl(&mut self, node_id: &NodeId) {
        self.negative_unl.remove(node_id);
    }

    /// Effective size: trusted minus negative UNL.
    pub fn effective_size(&self) -> usize {
        self.trusted
            .iter()
            .filter(|n| !self.negative_unl.contains(n))
            .count()
    }

    /// Quorum threshold: 80% of effective size, rounded up.
    pub fn quorum_threshold(&self) -> usize {
        let size = self.effective_size();
        if size == 0 {
            return 0;
        }
        (size * 80).div_ceil(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn node(id: u8) -> NodeId {
        NodeId(Hash256::new([id; 32]))
    }

    #[test]
    fn empty_unl_is_solo() {
        let unl = TrustedValidatorList::empty();
        assert!(unl.is_empty());
        assert_eq!(unl.effective_size(), 0);
        assert_eq!(unl.quorum_threshold(), 0);
    }

    #[test]
    fn trusted_check() {
        let mut trusted = HashSet::new();
        trusted.insert(node(1));
        trusted.insert(node(2));
        let unl = TrustedValidatorList::new(trusted);

        assert!(unl.is_trusted(&node(1)));
        assert!(unl.is_trusted(&node(2)));
        assert!(!unl.is_trusted(&node(3)));
    }

    #[test]
    fn negative_unl_reduces_effective_size() {
        let mut trusted = HashSet::new();
        for i in 1..=5 {
            trusted.insert(node(i));
        }
        let mut unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.effective_size(), 5);
        assert_eq!(unl.quorum_threshold(), 4); // ceil(5*0.8) = 4

        unl.add_to_negative_unl(node(1));
        assert_eq!(unl.effective_size(), 4);
        assert!(!unl.is_trusted(&node(1)));

        unl.remove_from_negative_unl(&node(1));
        assert_eq!(unl.effective_size(), 5);
        assert!(unl.is_trusted(&node(1)));
    }

    #[test]
    fn quorum_threshold_rounding() {
        // 10 validators -> 80% = 8
        let mut trusted = HashSet::new();
        for i in 1..=10 {
            trusted.insert(node(i));
        }
        let unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.quorum_threshold(), 8);

        // 3 validators -> ceil(3*0.8) = ceil(2.4) = 3
        let mut trusted = HashSet::new();
        for i in 1..=3 {
            trusted.insert(node(i));
        }
        let unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.quorum_threshold(), 3);
    }
}
