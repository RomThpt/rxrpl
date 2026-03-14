use std::sync::Arc;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::inner_node::InnerNode;
use crate::iterator::{SHAMapIter, SHAMapRefIter};
use crate::leaf_node::LeafNode;
use crate::node::{SHAMapNode, SHAMapState};
use crate::node_id::select_branch;
use crate::node_store::NodeStore;

/// A SHAMap is a merkle tree (16-way radix trie) keyed by 256-bit hashes.
///
/// It stores key-value pairs and provides deterministic root hashing
/// for consensus. The tree uses nibble-based (4-bit) branching, giving
/// a maximum depth of 64 levels for 256-bit keys.
///
/// The root is stored as `Arc<SHAMapNode>` so that `snapshot()` is O(1)
/// and copy-on-write only clones the modified path.
///
/// An optional `NodeStore` backend enables persistence. When a store is
/// attached, `flush()` writes all nodes to the store.
pub struct SHAMap {
    root: Arc<SHAMapNode>,
    state: SHAMapState,
    /// Factory for creating leaves of the right variant.
    leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    /// Optional backing store for persistence.
    store: Option<Arc<dyn NodeStore>>,
}

impl Clone for SHAMap {
    fn clone(&self) -> Self {
        Self {
            root: Arc::clone(&self.root),
            state: self.state,
            leaf_ctor: self.leaf_ctor,
            store: self.store.clone(),
        }
    }
}

impl std::fmt::Debug for SHAMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SHAMap")
            .field("state", &self.state)
            .field("root", &self.root)
            .field("has_store", &self.store.is_some())
            .finish()
    }
}

impl SHAMap {
    fn new_with_ctor(leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode) -> Self {
        Self {
            root: Arc::new(SHAMapNode::inner(InnerNode::new())),
            state: SHAMapState::Modifying,
            leaf_ctor,
            store: None,
        }
    }

    fn new_with_store(
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        store: Arc<dyn NodeStore>,
    ) -> Self {
        Self {
            root: Arc::new(SHAMapNode::inner(InnerNode::new())),
            state: SHAMapState::Modifying,
            leaf_ctor,
            store: Some(store),
        }
    }

    /// Create a new empty mutable SHAMap for account state.
    pub fn account_state() -> Self {
        Self::new_with_ctor(LeafNode::account_state)
    }

    /// Create a new empty mutable SHAMap for transactions (no metadata).
    pub fn transaction() -> Self {
        Self::new_with_ctor(LeafNode::transaction_no_meta)
    }

    /// Create a new empty mutable SHAMap for transactions with metadata.
    pub fn transaction_with_meta() -> Self {
        Self::new_with_ctor(LeafNode::transaction_with_meta)
    }

    /// Create a new empty mutable SHAMap for account state with a backing store.
    pub fn account_state_with_store(store: Arc<dyn NodeStore>) -> Self {
        Self::new_with_store(LeafNode::account_state, store)
    }

    /// Create a new empty mutable SHAMap for transactions with a backing store.
    pub fn transaction_with_store(store: Arc<dyn NodeStore>) -> Self {
        Self::new_with_store(LeafNode::transaction_no_meta, store)
    }

    /// Create a new empty mutable SHAMap for transactions with metadata and a backing store.
    pub fn transaction_with_meta_and_store(store: Arc<dyn NodeStore>) -> Self {
        Self::new_with_store(LeafNode::transaction_with_meta, store)
    }

    /// Attach a backing store to this map.
    pub fn set_store(&mut self, store: Arc<dyn NodeStore>) {
        self.store = Some(store);
    }

    /// Return a reference to the backing store, if any.
    pub fn store(&self) -> Option<&Arc<dyn NodeStore>> {
        self.store.as_ref()
    }

    /// Return the root hash of the tree, recomputing dirty nodes.
    pub fn root_hash(&mut self) -> Hash256 {
        Self::update_hashes(Arc::make_mut(&mut self.root));
        self.root.hash()
    }

    pub fn state(&self) -> SHAMapState {
        self.state
    }

    /// Make this map immutable.
    pub fn set_immutable(&mut self) {
        Self::update_hashes(Arc::make_mut(&mut self.root));
        self.state = SHAMapState::Immutable;
    }

    pub fn is_empty(&self) -> bool {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => inner.branch_count() == 0,
            SHAMapNode::Leaf(_) => false,
        }
    }

    /// Insert a key-value pair. Returns error if key already exists.
    pub fn insert(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        self.check_mutable()?;
        let leaf = (self.leaf_ctor)(key, data);
        let root = Self::root_inner_mut(&mut self.root);
        Self::insert_leaf(root, key, leaf, 0)
    }

    /// Get the data for a key.
    pub fn get(&self, key: &Hash256) -> Option<&[u8]> {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => Self::find_leaf(inner, key, 0).map(|l| l.data()),
            SHAMapNode::Leaf(leaf) => {
                if leaf.key() == key {
                    Some(leaf.data())
                } else {
                    None
                }
            }
        }
    }

    pub fn has(&self, key: &Hash256) -> bool {
        self.get(key).is_some()
    }

    /// Delete a key. Returns the old data.
    pub fn delete(&mut self, key: &Hash256) -> Result<Vec<u8>, SHAMapError> {
        self.check_mutable()?;
        let root = Self::root_inner_mut(&mut self.root);
        Self::delete_from(root, key, 0)
    }

    /// Update the data for an existing key.
    pub fn update(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        self.check_mutable()?;
        let root = Self::root_inner_mut(&mut self.root);
        Self::update_in(root, &key, data, 0)
    }

    /// Insert or update: if key exists, update; otherwise insert.
    pub fn put(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        self.check_mutable()?;
        let leaf = (self.leaf_ctor)(key, data);
        let root = Self::root_inner_mut(&mut self.root);
        Self::put_leaf(root, key, leaf, 0)
    }

    /// Visit all leaf nodes.
    pub fn for_each(&self, f: &mut impl FnMut(&Hash256, &[u8])) {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => Self::visit(inner, f),
            SHAMapNode::Leaf(leaf) => f(leaf.key(), leaf.data()),
        }
    }

    /// Create an immutable snapshot sharing the tree structure via Arc.
    pub fn snapshot(&mut self) -> SHAMap {
        Self::update_hashes(Arc::make_mut(&mut self.root));
        SHAMap {
            root: Arc::clone(&self.root),
            state: SHAMapState::Immutable,
            leaf_ctor: self.leaf_ctor,
            store: self.store.clone(),
        }
    }

    /// Return an owning iterator over all leaves (key, data).
    pub fn iter(&self) -> SHAMapIter {
        SHAMapIter::new(Arc::clone(&self.root))
    }

    /// Return a borrowing iterator over all leaves.
    pub fn iter_ref(&self) -> SHAMapRefIter<'_> {
        SHAMapRefIter::new(&self.root)
    }

    /// Create a mutable copy (even if the source is immutable).
    pub fn mutable_copy(&self) -> SHAMap {
        SHAMap {
            root: Arc::clone(&self.root),
            state: SHAMapState::Modifying,
            leaf_ctor: self.leaf_ctor,
            store: self.store.clone(),
        }
    }

    /// Flush all nodes (inner and leaf) to the backing store.
    ///
    /// Computes hashes for any dirty nodes, then serializes and stores
    /// all reachable nodes. Returns the root hash.
    ///
    /// No-op if no store is attached; returns the root hash regardless.
    pub fn flush(&mut self) -> Result<Hash256, SHAMapError> {
        Self::update_hashes(Arc::make_mut(&mut self.root));
        let root_hash = self.root.hash();

        if let Some(store) = &self.store {
            let mut entries = Vec::new();
            Self::collect_nodes(&self.root, &mut entries);
            if !entries.is_empty() {
                let refs: Vec<(&Hash256, &[u8])> =
                    entries.iter().map(|(h, d)| (h, d.as_slice())).collect();
                store.store_batch(&refs)?;
            }
        }

        Ok(root_hash)
    }

    /// Collect all node hashes and their serialized data for persistence.
    fn collect_nodes(node: &Arc<SHAMapNode>, out: &mut Vec<(Hash256, Vec<u8>)>) {
        match node.as_ref() {
            SHAMapNode::Inner(inner) => {
                let hash = inner.hash();
                if !hash.is_zero() {
                    // Serialize inner node as: [child_hash_0..child_hash_15]
                    let mut data = Vec::with_capacity(16 * 32);
                    for i in 0..16u8 {
                        data.extend_from_slice(inner.child_hash(i).as_bytes());
                    }
                    out.push((hash, data));
                }

                inner.for_each_branch(|_, child| {
                    Self::collect_nodes(child, out);
                });
            }
            SHAMapNode::Leaf(leaf) => {
                let hash = leaf.hash();
                // Serialize leaf as: [key || data]
                let mut data = Vec::with_capacity(32 + leaf.data().len());
                data.extend_from_slice(leaf.key().as_bytes());
                data.extend_from_slice(leaf.data());
                out.push((hash, data));
            }
        }
    }

    // --- Internal helpers ---

    fn check_mutable(&self) -> Result<(), SHAMapError> {
        if self.state == SHAMapState::Immutable {
            return Err(SHAMapError::Immutable);
        }
        Ok(())
    }

    /// Get a mutable reference to the root InnerNode, cloning via Arc::make_mut if shared.
    fn root_inner_mut(root: &mut Arc<SHAMapNode>) -> &mut InnerNode {
        match Arc::make_mut(root) {
            SHAMapNode::Inner(inner) => inner,
            _ => unreachable!("root must be an inner node"),
        }
    }

    fn find_leaf<'a>(node: &'a InnerNode, key: &Hash256, depth: u8) -> Option<&'a LeafNode> {
        let branch = select_branch(key, depth);
        let child = node.child(branch)?;
        match child.as_ref() {
            SHAMapNode::Leaf(leaf) => {
                if leaf.key() == key {
                    Some(leaf)
                } else {
                    None
                }
            }
            SHAMapNode::Inner(inner) => Self::find_leaf(inner, key, depth + 1),
        }
    }

    fn insert_leaf(
        node: &mut InnerNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, SHAMapNode::Leaf(leaf));
            return Ok(());
        }

        match node.take_child(branch) {
            Some(arc) => {
                // Try to unwrap, otherwise clone
                match Arc::try_unwrap(arc) {
                    Ok(owned) => Self::insert_into_existing(node, branch, owned, key, leaf, depth),
                    Err(shared) => {
                        let owned = (*shared).clone();
                        Self::insert_into_existing(node, branch, owned, key, leaf, depth)
                    }
                }
            }
            None => Ok(()),
        }
    }

    fn insert_into_existing(
        node: &mut InnerNode,
        branch: u8,
        existing: SHAMapNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        match existing {
            SHAMapNode::Leaf(existing_leaf) => {
                if existing_leaf.key() == &key {
                    node.set_child(branch, SHAMapNode::Leaf(existing_leaf));
                    return Err(SHAMapError::DuplicateKey);
                }
                let new_inner = Self::split_leaves(existing_leaf, leaf, depth + 1);
                node.set_child(branch, SHAMapNode::inner(new_inner));
                Ok(())
            }
            SHAMapNode::Inner(mut inner) => {
                Self::insert_leaf(&mut inner, key, leaf, depth + 1)?;
                node.set_child(branch, SHAMapNode::Inner(inner));
                Ok(())
            }
        }
    }

    fn put_leaf(
        node: &mut InnerNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, SHAMapNode::Leaf(leaf));
            return Ok(());
        }

        match node.take_child(branch) {
            Some(arc) => {
                let owned = match Arc::try_unwrap(arc) {
                    Ok(owned) => owned,
                    Err(shared) => (*shared).clone(),
                };
                match owned {
                    SHAMapNode::Leaf(existing_leaf) => {
                        if existing_leaf.key() == &key {
                            node.set_child(branch, SHAMapNode::Leaf(leaf));
                        } else {
                            let new_inner = Self::split_leaves(existing_leaf, leaf, depth + 1);
                            node.set_child(branch, SHAMapNode::inner(new_inner));
                        }
                        Ok(())
                    }
                    SHAMapNode::Inner(mut inner) => {
                        Self::put_leaf(&mut inner, key, leaf, depth + 1)?;
                        node.set_child(branch, SHAMapNode::Inner(inner));
                        Ok(())
                    }
                }
            }
            None => Ok(()),
        }
    }

    fn split_leaves(a: LeafNode, b: LeafNode, depth: u8) -> InnerNode {
        let mut inner = InnerNode::new();
        let branch_a = select_branch(a.key(), depth);
        let branch_b = select_branch(b.key(), depth);

        if branch_a == branch_b {
            let deeper = Self::split_leaves(a, b, depth + 1);
            inner.set_child(branch_a, SHAMapNode::inner(deeper));
        } else {
            inner.set_child(branch_a, SHAMapNode::Leaf(a));
            inner.set_child(branch_b, SHAMapNode::Leaf(b));
        }

        inner
    }

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
            Some(arc) => {
                let owned = match Arc::try_unwrap(arc) {
                    Ok(owned) => owned,
                    Err(shared) => (*shared).clone(),
                };
                match owned {
                    SHAMapNode::Leaf(leaf) => {
                        if leaf.key() == key {
                            Ok(leaf.data().to_vec())
                        } else {
                            node.set_child(branch, SHAMapNode::Leaf(leaf));
                            Err(SHAMapError::NotFound)
                        }
                    }
                    SHAMapNode::Inner(mut inner) => {
                        let data = Self::delete_from(&mut inner, key, depth + 1)?;
                        if inner.branch_count() == 0 {
                            // Empty inner, don't re-insert
                        } else if let Some(single) = inner.single_branch() {
                            if let Some(child_arc) = inner.take_child(single) {
                                let child = match Arc::try_unwrap(child_arc) {
                                    Ok(owned) => owned,
                                    Err(shared) => (*shared).clone(),
                                };
                                if matches!(&child, SHAMapNode::Leaf(_)) {
                                    node.set_child(branch, child);
                                } else {
                                    // Put it back and keep the inner
                                    inner.set_child(single, child);
                                    node.set_child(branch, SHAMapNode::Inner(inner));
                                }
                            }
                        } else {
                            node.set_child(branch, SHAMapNode::Inner(inner));
                        }
                        Ok(data)
                    }
                }
            }
            None => Err(SHAMapError::NotFound),
        }
    }

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
            Some(arc) => {
                let owned = match Arc::try_unwrap(arc) {
                    Ok(owned) => owned,
                    Err(shared) => (*shared).clone(),
                };
                match owned {
                    SHAMapNode::Leaf(mut leaf) => {
                        if leaf.key() == key {
                            leaf.update_data(data);
                            node.set_child(branch, SHAMapNode::Leaf(leaf));
                            Ok(())
                        } else {
                            node.set_child(branch, SHAMapNode::Leaf(leaf));
                            Err(SHAMapError::NotFound)
                        }
                    }
                    SHAMapNode::Inner(mut inner) => {
                        let result = Self::update_in(&mut inner, key, data, depth + 1);
                        node.set_child(branch, SHAMapNode::Inner(inner));
                        result
                    }
                }
            }
            None => Err(SHAMapError::NotFound),
        }
    }

    fn visit(node: &InnerNode, f: &mut impl FnMut(&Hash256, &[u8])) {
        node.for_each_branch(|_, child| match child.as_ref() {
            SHAMapNode::Leaf(leaf) => f(leaf.key(), leaf.data()),
            SHAMapNode::Inner(inner) => Self::visit(inner, f),
        });
    }

    /// Recursively update hashes for all dirty nodes.
    fn update_hashes(node: &mut SHAMapNode) {
        if let SHAMapNode::Inner(inner) = node {
            let mut mask = inner.branch_mask();
            while mask != 0 {
                let branch = mask.trailing_zeros() as u8;
                if let Some(child) = inner.child_mut(branch) {
                    Self::update_hashes(Arc::make_mut(child));
                }
                mask &= mask - 1;
            }
            inner.update_hash();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_store::InMemoryNodeStore;
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

        // Snapshot unchanged
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

    #[test]
    fn snapshot_cow_isolation() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        let snap_hash = map.root_hash();
        let snap = map.snapshot();

        // Modify original -- COW should kick in
        map.put(k1, vec![99]).unwrap();
        assert_eq!(snap.get(&k1), Some(&[1][..]));

        // Verify snapshot hash is preserved
        assert_eq!(snap.root.hash(), snap_hash);

        // Verify new hash differs
        let new_hash = map.root_hash();
        assert_ne!(new_hash, snap_hash);
    }

    #[test]
    fn mutable_copy_of_immutable() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();
        map.set_immutable();

        let mut copy = map.mutable_copy();
        assert_eq!(copy.state(), SHAMapState::Modifying);
        copy.put(k1, vec![2]).unwrap();
        assert_eq!(copy.get(&k1), Some(&[2][..]));
        // Original unchanged
        assert_eq!(map.get(&k1), Some(&[1][..]));
    }

    #[test]
    fn insert_order_independence() {
        let keys: Vec<Hash256> = (0..8u8)
            .map(|i| {
                let mut b = [0u8; 32];
                b[0] = i.wrapping_mul(17);
                b[1] = i.wrapping_mul(37);
                Hash256::new(b)
            })
            .collect();

        let mut map1 = SHAMap::transaction();
        for (i, k) in keys.iter().enumerate() {
            map1.put(*k, vec![i as u8]).unwrap();
        }

        let mut map2 = SHAMap::transaction();
        for (i, k) in keys.iter().enumerate().rev() {
            map2.put(*k, vec![i as u8]).unwrap();
        }

        assert_eq!(map1.root_hash(), map2.root_hash());
    }

    #[test]
    fn flush_without_store_returns_hash() {
        let mut map = SHAMap::account_state();
        let k = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(k, vec![1, 2, 3]).unwrap();

        let hash = map.flush().unwrap();
        assert!(!hash.is_zero());
        assert_eq!(hash, map.root_hash());
    }

    #[test]
    fn flush_with_store_persists_nodes() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());

        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        let root_hash = map.flush().unwrap();
        assert!(!root_hash.is_zero());

        // Root node should be stored
        let root_data = store.fetch(&root_hash).unwrap();
        assert!(root_data.is_some());
    }

    #[test]
    fn set_store_after_creation() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state();

        let k = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(k, vec![42]).unwrap();

        // Attach store after data is added
        map.set_store(store.clone());
        let root_hash = map.flush().unwrap();

        assert!(store.fetch(&root_hash).unwrap().is_some());
    }

    #[test]
    fn snapshot_inherits_store() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());
        let k = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(k, vec![1]).unwrap();

        let snap = map.snapshot();
        assert!(snap.store().is_some());
    }

    #[test]
    fn mutable_copy_inherits_store() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());
        let k = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(k, vec![1]).unwrap();
        map.set_immutable();

        let copy = map.mutable_copy();
        assert!(copy.store().is_some());
    }
}
