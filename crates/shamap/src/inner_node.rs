use std::sync::Arc;
use std::sync::OnceLock;

use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::leaf_node::LeafNode;
use crate::node::SHAMapNode;
use crate::node_id::BRANCH_FACTOR;
use crate::node_store::NodeStore;

/// A child slot that can be either loaded (node in memory) or unloaded (only hash known).
/// Uses `OnceLock` for thread-safe lazy initialization behind `&self`.
struct LazyChild {
    cell: OnceLock<Arc<SHAMapNode>>,
}

impl Clone for LazyChild {
    fn clone(&self) -> Self {
        match self.cell.get() {
            Some(node) => LazyChild {
                cell: OnceLock::from(Arc::clone(node)),
            },
            None => LazyChild {
                cell: OnceLock::new(),
            },
        }
    }
}

impl std::fmt::Debug for LazyChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyChild")
            .field("cell", &self.cell)
            .finish()
    }
}

impl Default for LazyChild {
    fn default() -> Self {
        LazyChild {
            cell: OnceLock::new(),
        }
    }
}

/// A 16-child branch node with Merkle hashing.
///
/// Uses `is_branch: u16` bitmask for fast empty-branch checks.
/// Children are `Arc<SHAMapNode>` for copy-on-write sharing.
#[derive(Clone, Debug)]
pub struct InnerNode {
    hashes: [Hash256; BRANCH_FACTOR],
    children: [LazyChild; BRANCH_FACTOR],
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

    /// Get a reference to a child node (only if already loaded, no lazy fetch).
    pub fn child(&self, branch: u8) -> Option<&Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.children[branch as usize].cell.get()
    }

    /// Get a reference to a child node, lazily loading from store if needed.
    pub fn child_with_store(
        &self,
        branch: u8,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<Option<&Arc<SHAMapNode>>, SHAMapError> {
        if self.is_empty_branch(branch) {
            return Ok(None);
        }
        if let Some(node) = self.children[branch as usize].cell.get() {
            return Ok(Some(node));
        }
        let hash = self.hashes[branch as usize];
        let store = store.ok_or(SHAMapError::MissingStore)?;
        let bytes = store
            .fetch(&hash)?
            .ok_or(SHAMapError::NodeNotFound(hash))?;
        let node = crate::node_store::deserialize_node(&bytes, &hash, leaf_ctor)?;
        let _ = self.children[branch as usize].cell.set(Arc::new(node));
        Ok(self.children[branch as usize].cell.get())
    }

    /// Get a reference to a child node that is already loaded (no lazy fetch).
    /// This is an alias for `child()` for clarity.
    pub fn child_loaded(&self, branch: u8) -> Option<&Arc<SHAMapNode>> {
        self.child(branch)
    }

    /// Get a mutable reference to a child Arc for copy-on-write.
    pub fn child_mut(&mut self, branch: u8) -> Option<&mut Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.children[branch as usize].cell.get_mut()
    }

    pub fn child_hash(&self, branch: u8) -> Hash256 {
        self.hashes[branch as usize]
    }

    /// Set a child node.
    pub fn set_child(&mut self, branch: u8, node: SHAMapNode) {
        let hash = node.hash();
        self.hashes[branch as usize] = hash;
        self.children[branch as usize] = LazyChild {
            cell: OnceLock::from(Arc::new(node)),
        };
        self.is_branch |= 1 << branch;
        self.invalidate_hash();
    }

    /// Set a child from an existing Arc.
    pub fn set_child_arc(&mut self, branch: u8, node: Arc<SHAMapNode>) {
        self.hashes[branch as usize] = node.hash();
        self.children[branch as usize] = LazyChild {
            cell: OnceLock::from(node),
        };
        self.is_branch |= 1 << branch;
        self.invalidate_hash();
    }

    /// Set only the child hash (node will be lazily loaded when accessed).
    pub fn set_child_hash(&mut self, branch: u8, hash: Hash256) {
        self.hashes[branch as usize] = hash;
        self.is_branch |= 1 << branch;
    }

    /// Set the cached hash directly, avoiding recomputation when loading from store.
    pub fn set_cached_hash(&mut self, hash: Hash256) {
        self.hash = hash;
    }

    /// Remove a child node.
    pub fn remove_child(&mut self, branch: u8) {
        self.hashes[branch as usize] = Hash256::ZERO;
        self.children[branch as usize] = LazyChild::default();
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

    /// Ensure a child is loaded into memory (for write paths that need take_child).
    pub fn ensure_loaded(
        &mut self,
        branch: u8,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<(), SHAMapError> {
        if self.is_empty_branch(branch) {
            return Ok(());
        }
        if self.children[branch as usize].cell.get().is_some() {
            return Ok(());
        }
        let hash = self.hashes[branch as usize];
        let store = store.ok_or(SHAMapError::MissingStore)?;
        let bytes = store
            .fetch(&hash)?
            .ok_or(SHAMapError::NodeNotFound(hash))?;
        let node = crate::node_store::deserialize_node(&bytes, &hash, leaf_ctor)?;
        let _ = self.children[branch as usize].cell.set(Arc::new(node));
        Ok(())
    }

    /// Take a child node out of this inner node (ownership transfer).
    pub fn take_child(&mut self, branch: u8) -> Option<Arc<SHAMapNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.hashes[branch as usize] = Hash256::ZERO;
        self.is_branch &= !(1 << branch);
        self.invalidate_hash();
        std::mem::take(&mut self.children[branch as usize])
            .cell
            .into_inner()
    }

    /// Iterate over all occupied branches.
    pub fn for_each_branch(&self, mut f: impl FnMut(u8, &Arc<SHAMapNode>)) {
        let mut mask = self.is_branch;
        while mask != 0 {
            let branch = mask.trailing_zeros() as u8;
            if let Some(child) = self.children[branch as usize].cell.get() {
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

    #[test]
    fn set_child_hash_marks_branch() {
        let mut node = InnerNode::new();
        let hash = Hash256::new([0xAA; 32]);
        node.set_child_hash(5, hash);
        assert!(!node.is_empty_branch(5));
        assert_eq!(node.child_hash(5), hash);
        // Node is not loaded yet
        assert!(node.child(5).is_none());
    }

    #[test]
    fn set_cached_hash() {
        let mut node = InnerNode::new();
        let hash = Hash256::new([0xBB; 32]);
        node.set_cached_hash(hash);
        assert_eq!(node.hash(), hash);
    }

    #[test]
    fn clone_preserves_loaded_children() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::account_state(Hash256::ZERO, vec![1, 2, 3]);
        node.set_child(5, SHAMapNode::Leaf(leaf));

        let cloned = node.clone();
        assert!(cloned.child(5).is_some());
        assert_eq!(cloned.child_hash(5), node.child_hash(5));
    }
}
