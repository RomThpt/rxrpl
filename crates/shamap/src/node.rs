use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

/// Hash prefixes used in SHAMap node hashing.
/// These are 4-byte prefixes prepended to data before SHA-512-Half.
pub mod hash_prefix {
    /// Inner node prefix: "MIN\0" (0x4D494E00)
    pub const INNER_NODE: [u8; 4] = [0x4D, 0x49, 0x4E, 0x00];

    /// Leaf node (account state) prefix: "MLN\0" (0x4D4C4E00)
    pub const LEAF_NODE: [u8; 4] = [0x4D, 0x4C, 0x4E, 0x00];

    /// Transaction ID prefix: "TXN\0" (0x54584E00)
    pub const TRANSACTION_ID: [u8; 4] = [0x54, 0x58, 0x4E, 0x00];

    /// Transaction node prefix: "SND\0" (0x534E4400)
    pub const TX_NODE: [u8; 4] = [0x53, 0x4E, 0x44, 0x00];
}

/// The type of data stored in a leaf node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafType {
    /// Account state / ledger entry.
    AccountState,
    /// Transaction without metadata.
    Transaction,
    /// Transaction with metadata.
    TransactionWithMeta,
}

/// A SHAMap tree node -- either an inner node or a leaf node.
#[derive(Clone, Debug)]
pub enum TreeNode {
    Inner(Box<InnerNode>),
    Leaf(LeafNode),
}

impl TreeNode {
    /// Get the cached hash of this node.
    pub fn hash(&self) -> Hash256 {
        match self {
            TreeNode::Inner(n) => n.hash(),
            TreeNode::Leaf(n) => n.hash,
        }
    }
}

/// An inner node with 16 possible children.
#[derive(Clone, Debug)]
pub struct InnerNode {
    children: [Option<Box<TreeNode>>; 16],
    hashes: [Hash256; 16],
    /// Bitmask of which branches are occupied.
    is_branch: u16,
    /// Cached hash of this node. Zero means dirty/needs recomputation.
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
            children: Default::default(),
            hashes: [Hash256::ZERO; 16],
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

    /// Compute the hash of this inner node.
    ///
    /// Hash = SHA-512-Half(prefix || hash0 || hash1 || ... || hash15)
    /// All 16 child hashes are included (zero for empty branches).
    fn compute_hash(&self) -> Hash256 {
        let mut data = Vec::with_capacity(4 + 16 * 32);
        data.extend_from_slice(&hash_prefix::INNER_NODE);
        for h in &self.hashes {
            data.extend_from_slice(h.as_bytes());
        }
        sha512_half(&[&data])
    }

    /// Update the hash after modifications.
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

    /// Check if a branch is empty.
    pub fn is_empty_branch(&self, branch: u8) -> bool {
        self.is_branch & (1 << branch) == 0
    }

    /// Get a reference to a child node.
    pub fn child(&self, branch: u8) -> Option<&TreeNode> {
        self.children[branch as usize].as_deref()
    }

    /// Get a mutable reference to a child node.
    pub fn child_mut(&mut self, branch: u8) -> Option<&mut TreeNode> {
        self.children[branch as usize].as_deref_mut()
    }

    /// Get the hash of a child.
    pub fn child_hash(&self, branch: u8) -> Hash256 {
        self.hashes[branch as usize]
    }

    /// Set a child node.
    pub fn set_child(&mut self, branch: u8, node: TreeNode) {
        let hash = node.hash();
        self.hashes[branch as usize] = hash;
        self.children[branch as usize] = Some(Box::new(node));
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
    pub fn take_child(&mut self, branch: u8) -> Option<Box<TreeNode>> {
        if self.is_empty_branch(branch) {
            return None;
        }
        self.hashes[branch as usize] = Hash256::ZERO;
        self.is_branch &= !(1 << branch);
        self.invalidate_hash();
        self.children[branch as usize].take()
    }

    /// Iterate over all occupied branches.
    pub fn for_each_branch(&self, mut f: impl FnMut(u8, &TreeNode)) {
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

/// A leaf node storing a key-value pair.
#[derive(Clone, Debug)]
pub struct LeafNode {
    pub key: Hash256,
    pub data: Vec<u8>,
    pub leaf_type: LeafType,
    pub hash: Hash256,
}

impl LeafNode {
    /// Create a new leaf node and compute its hash.
    pub fn new(key: Hash256, data: Vec<u8>, leaf_type: LeafType) -> Self {
        let hash = Self::compute_hash(&key, &data, leaf_type);
        Self {
            key,
            data,
            leaf_type,
            hash,
        }
    }

    /// Compute the hash for a leaf node based on its type.
    fn compute_hash(key: &Hash256, data: &[u8], leaf_type: LeafType) -> Hash256 {
        match leaf_type {
            LeafType::AccountState => {
                // SHA512Half(MLN\0 || data || key)
                sha512_half(&[&hash_prefix::LEAF_NODE, data, key.as_bytes()])
            }
            LeafType::Transaction => {
                // SHA512Half(TXN\0 || data)
                sha512_half(&[&hash_prefix::TRANSACTION_ID, data])
            }
            LeafType::TransactionWithMeta => {
                // SHA512Half(SND\0 || data || key)
                sha512_half(&[&hash_prefix::TX_NODE, data, key.as_bytes()])
            }
        }
    }

    /// Update the data and recompute the hash.
    pub fn update_data(&mut self, data: Vec<u8>) {
        self.hash = Self::compute_hash(&self.key, &data, self.leaf_type);
        self.data = data;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_node_empty() {
        let node = InnerNode::new();
        assert_eq!(node.branch_count(), 0);
        assert!(node.is_empty_branch(0));
        assert!(node.is_empty_branch(15));
    }

    #[test]
    fn inner_node_set_child() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::new(Hash256::ZERO, vec![1, 2, 3], LeafType::AccountState);
        let leaf_hash = leaf.hash;

        node.set_child(5, TreeNode::Leaf(leaf));
        assert!(!node.is_empty_branch(5));
        assert!(node.is_empty_branch(0));
        assert_eq!(node.branch_count(), 1);
        assert_eq!(node.child_hash(5), leaf_hash);
    }

    #[test]
    fn inner_node_remove_child() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::new(Hash256::ZERO, vec![1, 2, 3], LeafType::AccountState);
        node.set_child(3, TreeNode::Leaf(leaf));
        assert_eq!(node.branch_count(), 1);

        node.remove_child(3);
        assert_eq!(node.branch_count(), 0);
        assert!(node.is_empty_branch(3));
    }

    #[test]
    fn inner_node_single_branch() {
        let mut node = InnerNode::new();
        assert!(node.single_branch().is_none());

        let leaf = LeafNode::new(Hash256::ZERO, vec![1], LeafType::AccountState);
        node.set_child(7, TreeNode::Leaf(leaf));
        assert_eq!(node.single_branch(), Some(7));

        let leaf2 = LeafNode::new(Hash256::ZERO, vec![2], LeafType::AccountState);
        node.set_child(3, TreeNode::Leaf(leaf2));
        assert!(node.single_branch().is_none());
    }

    #[test]
    fn inner_node_hash_deterministic() {
        let mut node = InnerNode::new();
        let leaf = LeafNode::new(Hash256::ZERO, vec![1, 2, 3], LeafType::AccountState);
        node.set_child(0, TreeNode::Leaf(leaf));
        node.update_hash();
        let h1 = node.hash();

        let mut node2 = InnerNode::new();
        let leaf2 = LeafNode::new(Hash256::ZERO, vec![1, 2, 3], LeafType::AccountState);
        node2.set_child(0, TreeNode::Leaf(leaf2));
        node2.update_hash();
        let h2 = node2.hash();

        assert_eq!(h1, h2);
        assert!(!h1.is_zero());
    }

    #[test]
    fn leaf_hash_account_state() {
        let key = Hash256::ZERO;
        let data = vec![1, 2, 3, 4];
        let leaf = LeafNode::new(key, data.clone(), LeafType::AccountState);
        assert!(!leaf.hash.is_zero());

        // Same inputs produce same hash
        let leaf2 = LeafNode::new(key, data, LeafType::AccountState);
        assert_eq!(leaf.hash, leaf2.hash);
    }

    #[test]
    fn leaf_types_produce_different_hashes() {
        let key = Hash256::ZERO;
        let data = vec![1, 2, 3, 4];
        let h1 = LeafNode::new(key, data.clone(), LeafType::AccountState).hash;
        let h2 = LeafNode::new(key, data.clone(), LeafType::Transaction).hash;
        let h3 = LeafNode::new(key, data, LeafType::TransactionWithMeta).hash;
        assert_ne!(h1, h2);
        assert_ne!(h1, h3);
        assert_ne!(h2, h3);
    }

    #[test]
    fn leaf_update_data() {
        let key = Hash256::ZERO;
        let mut leaf = LeafNode::new(key, vec![1, 2, 3], LeafType::AccountState);
        let old_hash = leaf.hash;
        leaf.update_data(vec![4, 5, 6]);
        assert_ne!(leaf.hash, old_hash);
    }
}
