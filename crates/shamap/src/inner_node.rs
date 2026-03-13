use std::sync::Arc;

use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

use crate::node::SHAMapNode;
use crate::node_id::BRANCH_FACTOR;

/// A 16-child branch node with Merkle hashing.
///
/// Uses `is_branch: u16` bitmask for fast empty-branch checks.
/// Children are `Arc<SHAMapNode>` for copy-on-write sharing.
#[derive(Clone, Debug)]
pub struct InnerNode {
    hashes: [Hash256; BRANCH_FACTOR],
    children: [Option<Arc<SHAMapNode>>; BRANCH_FACTOR],
    is_branch: u16,
    hash: Hash256,
}

impl Default for InnerNode {
    fn default() -> Self {
        Self::new()
    }
}

impl InnerNode {
    pub fn new() -> Self {
        Self {
            hashes: [Hash256::ZERO; BRANCH_FACTOR],
            children: Default::default(),
            is_branch: 0,
            hash: Hash256::ZERO,
        }
    }

    /// Return the cached hash, recomputing if dirty.
    pub fn hash(&self) -> Hash256 {
        if self.hash.is_zero() && self.is_branch != 0 {
            self.compute_hash()
        } else {
            self.hash
        }
    }

    /// Hash = SHA-512-Half(INNER_NODE || hash[0] || hash[1] || ... || hash[15])
    fn compute_hash(&self) -> Hash256 {
        let prefix = HashPrefix::INNER_NODE.to_bytes();
        let mut data = Vec::with_capacity(4 + BRANCH_FACTOR * 32);
        data.extend_from_slice(&prefix);
        for h in &self.hashes {
            data.extend_from_slice(h.as_bytes());
        }
        sha512_half(&[&data])
    }

    /// Recompute and cache the hash.
    pub fn update_hash(&mut self) {
        if self.is_branch == 0 {
            self.hash = Hash256::ZERO;
        } else {
            self.hash = self.compute_hash();
        }
    }

    /// Mark hash as dirty (needs recomputation).
    pub fn invalidate_hash(&mut self) {
        self.hash = Hash256::ZERO;
    }

    pub fn is_empty_branch(&self, branch: u8) -> bool {
        self.is_branch & (1 << branch) == 0
    }

    /// Get a reference to a child node.
    pub fn child(&self, branch: u8) -> Option<&Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.children[branch as usize].as_ref()
    }

    /// Get a mutable reference to a child Arc for copy-on-write.
    pub fn child_mut(&mut self, branch: u8) -> Option<&mut Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.children[branch as usize].as_mut()
    }

    pub fn child_hash(&self, branch: u8) -> Hash256 {
        self.hashes[branch as usize]
    }

    /// Set a child node.
    pub fn set_child(&mut self, branch: u8, node: SHAMapNode) {
        let hash = node.hash();
        self.hashes[branch as usize] = hash;
        self.children[branch as usize] = Some(Arc::new(node));
        self.is_branch |= 1 << branch;
        self.invalidate_hash();
    }

    /// Set a child from an existing Arc.
    pub fn set_child_arc(&mut self, branch: u8, node: Arc<SHAMapNode>) {
        self.hashes[branch as usize] = node.hash();
        self.children[branch as usize] = Some(node);
        self.is_branch |= 1 << branch;
        self.invalidate_hash();
    }

    /// Remove a child node.
    pub fn remove_child(&mut self, branch: u8) {
        self.hashes[branch as usize] = Hash256::ZERO;
        self.children[branch as usize] = None;
        self.is_branch &= !(1 << branch);
        self.invalidate_hash();
    }

    /// Return the raw branch bitmask.
    pub fn branch_mask(&self) -> u16 {
        self.is_branch
    }

    /// Count the number of occupied branches.
    pub fn branch_count(&self) -> u32 {
        self.is_branch.count_ones()
    }

    /// Find the single occupied branch, if exactly one exists.
    pub fn single_branch(&self) -> Option<u8> {
        if self.branch_count() == 1 {
            Some(self.is_branch.trailing_zeros() as u8)
        } else {
            None
        }
    }

    /// Take a child node out of this inner node (ownership transfer).
    pub fn take_child(&mut self, branch: u8) -> Option<Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.hashes[branch as usize] = Hash256::ZERO;
        self.is_branch &= !(1 << branch);
        self.invalidate_hash();
        self.children[branch as usize].take()
    }

    /// Iterate over all occupied branches.
    pub fn for_each_branch(&self, mut f: impl FnMut(u8, &Arc<SHAMapNode>)) {
        let mut mask = self.is_branch;
        while mask != 0 {
            let branch = mask.trailing_zeros() as u8;
            if let Some(child) = &self.children[branch as usize] {
                f(branch, child);
            }
            mask &= mask - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leaf_node::LeafNode;

    #[test]
    fn empty_inner_node() {
        let node = InnerNode::new();
        assert_eq!(node.branch_count(), 0);
        assert!(node.is_empty_branch(0));
        assert!(node.is_empty_branch(15));
        assert_eq!(node.hash(), Hash256::ZERO);
    }

    #[test]
    fn set_and_get_child() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::account_state(Hash256::ZERO, vec![1, 2, 3]);
        let leaf_hash = leaf.hash();

        node.set_child(5, SHAMapNode::Leaf(leaf));
        assert!(!node.is_empty_branch(5));
        assert!(node.is_empty_branch(0));
        assert_eq!(node.branch_count(), 1);
        assert_eq!(node.child_hash(5), leaf_hash);
    }

    #[test]
    fn remove_child() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::account_state(Hash256::ZERO, vec![1, 2, 3]);
        node.set_child(3, SHAMapNode::Leaf(leaf));
        assert_eq!(node.branch_count(), 1);

        node.remove_child(3);
        assert_eq!(node.branch_count(), 0);
        assert!(node.is_empty_branch(3));
    }

    #[test]
    fn single_branch_detection() {
        let mut node = InnerNode::new();
        assert!(node.single_branch().is_none());

        let leaf = LeafNode::account_state(Hash256::ZERO, vec![1]);
        node.set_child(7, SHAMapNode::Leaf(leaf));
        assert_eq!(node.single_branch(), Some(7));

        let leaf2 = LeafNode::account_state(Hash256::new([1; 32]), vec![2]);
        node.set_child(3, SHAMapNode::Leaf(leaf2));
        assert!(node.single_branch().is_none());
    }

    #[test]
    fn hash_deterministic() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::account_state(Hash256::ZERO, vec![1, 2, 3]);
        node.set_child(0, SHAMapNode::Leaf(leaf));
        node.update_hash();
        let h1 = node.hash();

        let mut node2 = InnerNode::new();
        let leaf2 = LeafNode::account_state(Hash256::ZERO, vec![1, 2, 3]);
        node2.set_child(0, SHAMapNode::Leaf(leaf2));
        node2.update_hash();
        let h2 = node2.hash();

        assert_eq!(h1, h2);
        assert!(!h1.is_zero());
    }

    #[test]
    fn hash_changes_with_children() {
        let mut node = InnerNode::new();
        let leaf1 = LeafNode::account_state(Hash256::ZERO, vec![1]);
        node.set_child(0, SHAMapNode::Leaf(leaf1));
        node.update_hash();
        let h1 = node.hash();

        let leaf2 = LeafNode::account_state(Hash256::new([1; 32]), vec![2]);
        node.set_child(1, SHAMapNode::Leaf(leaf2));
        node.update_hash();
        let h2 = node.hash();

        assert_ne!(h1, h2);
    }
}
