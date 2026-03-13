use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::node::{InnerNode, LeafNode, LeafType, TreeNode};
use crate::node_id::select_branch;

/// The state of a SHAMap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SHAMapState {
    /// Open for changes.
    Modifying,
    /// Frozen, no changes allowed.
    Immutable,
}

/// A SHAMap is a merkle tree (16-way radix trie) keyed by 256-bit hashes.
///
/// It stores key-value pairs and provides deterministic root hashing
/// for consensus. The tree uses nibble-based (4-bit) branching, giving
/// a maximum depth of 64 levels for 256-bit keys.
#[derive(Clone, Debug)]
pub struct SHAMap {
    root: InnerNode,
    state: SHAMapState,
    leaf_type: LeafType,
}

impl SHAMap {
    /// Create a new empty mutable SHAMap for the given leaf type.
    pub fn new(leaf_type: LeafType) -> Self {
        Self {
            root: InnerNode::new(),
            state: SHAMapState::Modifying,
            leaf_type,
        }
    }

    /// Create a new empty mutable SHAMap for account state.
    pub fn account_state() -> Self {
        Self::new(LeafType::AccountState)
    }

    /// Create a new empty mutable SHAMap for transactions.
    pub fn transaction() -> Self {
        Self::new(LeafType::Transaction)
    }

    /// Create a new empty mutable SHAMap for transactions with metadata.
    pub fn transaction_with_meta() -> Self {
        Self::new(LeafType::TransactionWithMeta)
    }

    /// Return the root hash of the tree.
    pub fn root_hash(&mut self) -> Hash256 {
        self.root.update_hash();
        self.root.hash()
    }

    /// Return the current state.
    pub fn state(&self) -> SHAMapState {
        self.state
    }

    /// Make this map immutable.
    pub fn set_immutable(&mut self) {
        self.root.update_hash();
        self.state = SHAMapState::Immutable;
    }

    /// Return true if the map is empty (no items).
    pub fn is_empty(&self) -> bool {
        self.root.branch_count() == 0
    }

    /// Insert a key-value pair. Returns error if key already exists.
    pub fn insert(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        if self.state == SHAMapState::Immutable {
            return Err(SHAMapError::Immutable);
        }
        let leaf = LeafNode::new(key, data, self.leaf_type);
        Self::insert_leaf(&mut self.root, key, leaf, 0)
    }

    /// Get the data for a key, if it exists.
    pub fn get(&self, key: &Hash256) -> Option<&[u8]> {
        Self::find_leaf(&self.root, key, 0).map(|leaf| leaf.data.as_slice())
    }

    /// Check if a key exists.
    pub fn has(&self, key: &Hash256) -> bool {
        self.get(key).is_some()
    }

    /// Delete a key. Returns the old data if it existed.
    pub fn delete(&mut self, key: &Hash256) -> Result<Vec<u8>, SHAMapError> {
        if self.state == SHAMapState::Immutable {
            return Err(SHAMapError::Immutable);
        }
        Self::delete_from(&mut self.root, key, 0)
    }

    /// Update the data for an existing key.
    pub fn update(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        if self.state == SHAMapState::Immutable {
            return Err(SHAMapError::Immutable);
        }
        Self::update_in(&mut self.root, &key, data, 0)
    }

    /// Insert or update: if key exists, update; otherwise insert.
    pub fn put(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        if self.state == SHAMapState::Immutable {
            return Err(SHAMapError::Immutable);
        }
        let leaf = LeafNode::new(key, data, self.leaf_type);
        Self::put_leaf(&mut self.root, key, leaf, 0)
    }

    /// Visit all leaf nodes in order.
    pub fn for_each(&self, f: &mut impl FnMut(&Hash256, &[u8])) {
        Self::visit(&self.root, f);
    }

    /// Create an immutable snapshot of this map.
    pub fn snapshot(&mut self) -> SHAMap {
        self.root.update_hash();
        let mut snap = self.clone();
        snap.state = SHAMapState::Immutable;
        snap
    }

    /// Create a mutable copy of this map.
    ///
    /// Even if the source map is immutable, the copy is mutable.
    /// Used when deriving a new open ledger from a closed parent.
    pub fn mutable_copy(&self) -> SHAMap {
        let mut copy = self.clone();
        copy.state = SHAMapState::Modifying;
        copy
    }

    // --- Internal helpers (all static to avoid borrow issues) ---

    /// Find a leaf by key, descending from the given inner node.
    fn find_leaf<'a>(node: &'a InnerNode, key: &Hash256, depth: u8) -> Option<&'a LeafNode> {
        let branch = select_branch(key, depth);
        match node.child(branch)? {
            TreeNode::Leaf(leaf) => {
                if leaf.key == *key {
                    Some(leaf)
                } else {
                    None
                }
            }
            TreeNode::Inner(inner) => Self::find_leaf(inner, key, depth + 1),
        }
    }

    /// Insert a leaf into the tree. Handles splitting when two keys collide.
    fn insert_leaf(
        node: &mut InnerNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, TreeNode::Leaf(leaf));
            return Ok(());
        }

        match node.take_child(branch) {
            Some(boxed) => match *boxed {
                TreeNode::Leaf(existing) => {
                    if existing.key == key {
                        node.set_child(branch, TreeNode::Leaf(existing));
                        return Err(SHAMapError::DuplicateKey);
                    }
                    let new_inner = Self::split_leaves(existing, leaf, depth + 1);
                    node.set_child(branch, TreeNode::Inner(Box::new(new_inner)));
                    Ok(())
                }
                TreeNode::Inner(mut inner) => {
                    Self::insert_leaf(&mut inner, key, leaf, depth + 1)?;
                    node.set_child(branch, TreeNode::Inner(inner));
                    Ok(())
                }
            },
            None => Ok(()),
        }
    }

    /// Put (upsert) a leaf into the tree.
    fn put_leaf(
        node: &mut InnerNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, TreeNode::Leaf(leaf));
            return Ok(());
        }

        match node.take_child(branch) {
            Some(boxed) => match *boxed {
                TreeNode::Leaf(existing) => {
                    if existing.key == key {
                        node.set_child(branch, TreeNode::Leaf(leaf));
                        Ok(())
                    } else {
                        let new_inner = Self::split_leaves(existing, leaf, depth + 1);
                        node.set_child(branch, TreeNode::Inner(Box::new(new_inner)));
                        Ok(())
                    }
                }
                TreeNode::Inner(mut inner) => {
                    Self::put_leaf(&mut inner, key, leaf, depth + 1)?;
                    node.set_child(branch, TreeNode::Inner(inner));
                    Ok(())
                }
            },
            None => Ok(()),
        }
    }

    /// Create inner nodes to separate two leaves that share a common prefix.
    fn split_leaves(a: LeafNode, b: LeafNode, depth: u8) -> InnerNode {
        let mut inner = InnerNode::new();
        let branch_a = select_branch(&a.key, depth);
        let branch_b = select_branch(&b.key, depth);

        if branch_a == branch_b {
            let deeper = Self::split_leaves(a, b, depth + 1);
            inner.set_child(branch_a, TreeNode::Inner(Box::new(deeper)));
        } else {
            inner.set_child(branch_a, TreeNode::Leaf(a));
            inner.set_child(branch_b, TreeNode::Leaf(b));
        }

        inner
    }

    /// Delete a key from the subtree rooted at `node`.
    fn delete_from(
        node: &mut InnerNode,
        key: &Hash256,
        depth: u8,
    ) -> Result<Vec<u8>, SHAMapError> {
        let branch = select_branch(key, depth);

        if node.is_empty_branch(branch) {
            return Err(SHAMapError::NotFound);
        }

        match node.take_child(branch) {
            Some(boxed) => match *boxed {
                TreeNode::Leaf(leaf) => {
                    if leaf.key == *key {
                        Ok(leaf.data)
                    } else {
                        node.set_child(branch, TreeNode::Leaf(leaf));
                        Err(SHAMapError::NotFound)
                    }
                }
                TreeNode::Inner(mut inner) => {
                    let data = Self::delete_from(&mut inner, key, depth + 1)?;
                    if inner.branch_count() == 0 {
                        // Inner is empty, don't re-insert
                    } else if let Some(single) = inner.single_branch() {
                        // Collapse: if single child is a leaf, pull it up
                        if let Some(child) = inner.take_child(single) {
                            if matches!(*child, TreeNode::Leaf(_)) {
                                node.set_child(branch, *child);
                            } else {
                                inner.set_child(single, *child);
                                node.set_child(branch, TreeNode::Inner(inner));
                            }
                        }
                    } else {
                        node.set_child(branch, TreeNode::Inner(inner));
                    }
                    Ok(data)
                }
            },
            None => Err(SHAMapError::NotFound),
        }
    }

    /// Update data for a key in the subtree.
    fn update_in(
        node: &mut InnerNode,
        key: &Hash256,
        data: Vec<u8>,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(key, depth);

        if node.is_empty_branch(branch) {
            return Err(SHAMapError::NotFound);
        }

        match node.take_child(branch) {
            Some(boxed) => match *boxed {
                TreeNode::Leaf(mut leaf) => {
                    if leaf.key == *key {
                        leaf.update_data(data);
                        node.set_child(branch, TreeNode::Leaf(leaf));
                        Ok(())
                    } else {
                        node.set_child(branch, TreeNode::Leaf(leaf));
                        Err(SHAMapError::NotFound)
                    }
                }
                TreeNode::Inner(mut inner) => {
                    let result = Self::update_in(&mut inner, key, data, depth + 1);
                    node.set_child(branch, TreeNode::Inner(inner));
                    result
                }
            },
            None => Err(SHAMapError::NotFound),
        }
    }

    /// Visit all leaves in the subtree in a depth-first traversal.
    fn visit(node: &InnerNode, f: &mut impl FnMut(&Hash256, &[u8])) {
        node.for_each_branch(|_, child| match child {
            TreeNode::Leaf(leaf) => f(&leaf.key, &leaf.data),
            TreeNode::Inner(inner) => Self::visit(inner, f),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn make_key(hex: &str) -> Hash256 {
        Hash256::from_str(hex).unwrap()
    }

    #[test]
    fn empty_map() {
        let mut map = SHAMap::account_state();
        assert!(map.is_empty());
        assert_eq!(map.root_hash(), Hash256::ZERO);
    }

    #[test]
    fn put_and_get() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let data = vec![1, 2, 3, 4];

        map.put(key, data.clone()).unwrap();
        assert!(!map.is_empty());
        assert_eq!(map.get(&key), Some(data.as_slice()));
        assert!(map.has(&key));
    }

    #[test]
    fn get_nonexistent() {
        let map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(map.get(&key), None);
        assert!(!map.has(&key));
    }

    #[test]
    fn put_update() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

        map.put(key, vec![1, 2, 3]).unwrap();
        map.put(key, vec![4, 5, 6]).unwrap();
        assert_eq!(map.get(&key), Some(&[4, 5, 6][..]));
    }

    #[test]
    fn insert_duplicate_error() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

        map.insert(key, vec![1, 2, 3]).unwrap();
        assert_eq!(
            map.insert(key, vec![4, 5, 6]),
            Err(SHAMapError::DuplicateKey)
        );
    }

    #[test]
    fn delete_existing() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        let data = vec![1, 2, 3];

        map.put(key, data.clone()).unwrap();
        let removed = map.delete(&key).unwrap();
        assert_eq!(removed, data);
        assert!(!map.has(&key));
        assert!(map.is_empty());
    }

    #[test]
    fn delete_nonexistent() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(map.delete(&key), Err(SHAMapError::NotFound));
    }

    #[test]
    fn multiple_keys() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 =
            make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 =
            make_key("3000000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();
        map.put(k3, vec![3]).unwrap();

        assert_eq!(map.get(&k1), Some(&[1][..]));
        assert_eq!(map.get(&k2), Some(&[2][..]));
        assert_eq!(map.get(&k3), Some(&[3][..]));
    }

    #[test]
    fn keys_sharing_prefix() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("A100000000000000000000000000000000000000000000000000000000000000");
        let k2 =
            make_key("A200000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        assert_eq!(map.get(&k1), Some(&[1][..]));
        assert_eq!(map.get(&k2), Some(&[2][..]));
    }

    #[test]
    fn keys_sharing_long_prefix() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("ABCDEF0000000000000000000000000000000000000000000000000000000001");
        let k2 =
            make_key("ABCDEF0000000000000000000000000000000000000000000000000000000002");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        assert_eq!(map.get(&k1), Some(&[1][..]));
        assert_eq!(map.get(&k2), Some(&[2][..]));
    }

    #[test]
    fn root_hash_changes_on_modification() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");

        let h0 = map.root_hash();
        map.put(k1, vec![1]).unwrap();
        let h1 = map.root_hash();
        assert_ne!(h0, h1);

        map.put(k1, vec![2]).unwrap();
        let h2 = map.root_hash();
        assert_ne!(h1, h2);
    }

    #[test]
    fn root_hash_deterministic() {
        let k1 =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 =
            make_key("2000000000000000000000000000000000000000000000000000000000000000");

        let mut map1 = SHAMap::account_state();
        map1.put(k1, vec![1]).unwrap();
        map1.put(k2, vec![2]).unwrap();

        let mut map2 = SHAMap::account_state();
        map2.put(k2, vec![2]).unwrap();
        map2.put(k1, vec![1]).unwrap();

        assert_eq!(map1.root_hash(), map2.root_hash());
    }

    #[test]
    fn for_each_visits_all() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 =
            make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 =
            make_key("3000000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();
        map.put(k3, vec![3]).unwrap();

        let mut visited = Vec::new();
        map.for_each(&mut |key, data| {
            visited.push((*key, data.to_vec()));
        });

        assert_eq!(visited.len(), 3);
    }

    #[test]
    fn immutable_rejects_modifications() {
        let mut map = SHAMap::account_state();
        map.set_immutable();

        let key =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(map.put(key, vec![1]), Err(SHAMapError::Immutable));
        assert_eq!(map.delete(&key), Err(SHAMapError::Immutable));
    }

    #[test]
    fn snapshot_is_independent() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("1000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();

        let snap = map.snapshot();

        // Modify original
        map.put(k1, vec![2]).unwrap();

        // Snapshot should be unchanged
        assert_eq!(snap.get(&k1), Some(&[1][..]));
        assert_eq!(snap.state(), SHAMapState::Immutable);
    }

    #[test]
    fn update_existing() {
        let mut map = SHAMap::account_state();
        let key =
            make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

        map.put(key, vec![1]).unwrap();
        map.update(key, vec![9, 9, 9]).unwrap();
        assert_eq!(map.get(&key), Some(&[9, 9, 9][..]));
    }

    #[test]
    fn update_nonexistent_fails() {
        let mut map = SHAMap::account_state();
        let key =
            make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(map.update(key, vec![1]), Err(SHAMapError::NotFound));
    }

    #[test]
    fn delete_with_consolidation() {
        let mut map = SHAMap::account_state();
        let k1 =
            make_key("A100000000000000000000000000000000000000000000000000000000000000");
        let k2 =
            make_key("A200000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        map.delete(&k1).unwrap();
        assert_eq!(map.get(&k2), Some(&[2][..]));
        assert!(!map.has(&k1));
    }

    #[test]
    fn many_items() {
        let mut map = SHAMap::account_state();
        let mut keys = Vec::new();

        for i in 0u8..100 {
            let mut key_bytes = [0u8; 32];
            key_bytes[0] = i;
            key_bytes[1] = i.wrapping_mul(37);
            let key = Hash256::new(key_bytes);
            map.put(key, vec![i]).unwrap();
            keys.push(key);
        }

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(map.get(key), Some(&[i as u8][..]), "missing key {i}");
        }

        let mut count = 0;
        map.for_each(&mut |_, _| count += 1);
        assert_eq!(count, 100);

        assert!(!map.root_hash().is_zero());
    }
}
