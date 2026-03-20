use std::sync::Arc;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::inner_node::InnerNode;
use crate::iterator::{SHAMapIter, SHAMapRefIter};
use crate::leaf_node::LeafNode;
use crate::node::{SHAMapNode, SHAMapState};
use crate::node_id::select_branch;
use crate::node_store::NodeStore;

/// A difference entry between two SHAMaps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffEntry {
    /// Key exists in the new map but not the old.
    Added { key: Hash256 },
    /// Key exists in the old map but not the new.
    Removed { key: Hash256 },
    /// Key exists in both maps but with different data.
    Modified { key: Hash256 },
}

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
        let store = self.store.clone();
        let leaf_ctor = self.leaf_ctor;
        let root = Self::root_inner_mut(&mut self.root);
        Self::insert_leaf(root, key, leaf, 0, store.as_ref(), leaf_ctor)
    }

    /// Get the data for a key.
    pub fn get(&self, key: &Hash256) -> Option<&[u8]> {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => {
                Self::find_leaf(inner, key, 0, self.store.as_ref(), self.leaf_ctor)
                    .map(|l| l.data())
            }
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
        let store = self.store.clone();
        let leaf_ctor = self.leaf_ctor;
        let root = Self::root_inner_mut(&mut self.root);
        Self::delete_from(root, key, 0, store.as_ref(), leaf_ctor)
    }

    /// Update the data for an existing key.
    pub fn update(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        self.check_mutable()?;
        let store = self.store.clone();
        let leaf_ctor = self.leaf_ctor;
        let root = Self::root_inner_mut(&mut self.root);
        Self::update_in(root, &key, data, 0, store.as_ref(), leaf_ctor)
    }

    /// Insert or update: if key exists, update; otherwise insert.
    pub fn put(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), SHAMapError> {
        self.check_mutable()?;
        let leaf = (self.leaf_ctor)(key, data);
        let store = self.store.clone();
        let leaf_ctor = self.leaf_ctor;
        let root = Self::root_inner_mut(&mut self.root);
        Self::put_leaf(root, key, leaf, 0, store.as_ref(), leaf_ctor)
    }

    /// Visit all leaf nodes.
    pub fn for_each(&self, f: &mut impl FnMut(&Hash256, &[u8])) {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => {
                Self::visit(inner, f, self.store.as_ref(), self.leaf_ctor)
            }
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
        SHAMapIter::new(Arc::clone(&self.root), self.store.clone(), self.leaf_ctor)
    }

    /// Return a borrowing iterator over all leaves.
    pub fn iter_ref(&self) -> SHAMapRefIter<'_> {
        SHAMapRefIter::new(&self.root, self.store.as_ref(), self.leaf_ctor)
    }

    /// Create a SHAMap from a root hash, loading nodes lazily from the store.
    ///
    /// Only the root node is fetched immediately. Children are loaded on demand
    /// when traversed via `get()`, `for_each()`, iterators, etc.
    pub fn from_root_hash(
        root_hash: Hash256,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        store: Arc<dyn NodeStore>,
    ) -> Result<Self, SHAMapError> {
        let bytes = store
            .fetch(&root_hash)?
            .ok_or(SHAMapError::NodeNotFound(root_hash))?;
        let root_node = crate::node_store::deserialize_node(&bytes, &root_hash, leaf_ctor)?;
        Ok(Self {
            root: Arc::new(root_node),
            state: SHAMapState::Modifying,
            leaf_ctor,
            store: Some(store),
        })
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

    /// Flush modified nodes to the backing store (incremental).
    ///
    /// Only dirty subtrees (nodes that were modified since last flush) are
    /// serialized and persisted. Unmodified nodes loaded from the store are
    /// skipped since they are already persisted.
    ///
    /// No-op if no store is attached; returns the root hash regardless.
    pub fn flush(&mut self) -> Result<Hash256, SHAMapError> {
        Self::update_hashes(Arc::make_mut(&mut self.root));
        let root_hash = self.root.hash();

        if let Some(store) = &self.store {
            let mut entries = Vec::new();
            Self::collect_dirty_nodes(&self.root, &mut entries);
            if !entries.is_empty() {
                let refs: Vec<(&Hash256, &[u8])> =
                    entries.iter().map(|(h, d)| (h, d.as_slice())).collect();
                store.store_batch(&refs)?;
            }
        }

        Ok(root_hash)
    }

    /// Collect dirty node hashes and their serialized data for persistence.
    ///
    /// A node is considered "dirty" if it was loaded in memory (and potentially
    /// modified). Unloaded nodes (only hash known) are already in the store.
    fn collect_dirty_nodes(node: &Arc<SHAMapNode>, out: &mut Vec<(Hash256, Vec<u8>)>) {
        match node.as_ref() {
            SHAMapNode::Inner(inner) => {
                let hash = inner.hash();
                if !hash.is_zero() {
                    let mut data = Vec::with_capacity(16 * 32);
                    for i in 0..16u8 {
                        data.extend_from_slice(inner.child_hash(i).as_bytes());
                    }
                    out.push((hash, data));
                }

                // for_each_branch only visits loaded children
                inner.for_each_branch(|_, child| {
                    Self::collect_dirty_nodes(child, out);
                });
            }
            SHAMapNode::Leaf(leaf) => {
                let hash = leaf.hash();
                let mut data = Vec::with_capacity(32 + leaf.data().len());
                data.extend_from_slice(leaf.key().as_bytes());
                data.extend_from_slice(leaf.data());
                out.push((hash, data));
            }
        }
    }

    /// Find differences between this map and another.
    ///
    /// Returns a list of `DiffEntry` describing keys that were added, removed,
    /// or modified between `self` (the "old" state) and `other` (the "new" state).
    ///
    /// Uses Merkle hashes to skip identical subtrees, making this efficient
    /// for maps that share most of their data (e.g., consecutive ledger states).
    pub fn find_difference(&self, other: &SHAMap) -> Vec<DiffEntry> {
        let mut diffs = Vec::new();
        Self::diff_nodes(
            &self.root,
            &other.root,
            &mut diffs,
            self.store.as_ref(),
            other.store.as_ref(),
            self.leaf_ctor,
            other.leaf_ctor,
        );
        diffs
    }

    fn diff_nodes(
        old: &Arc<SHAMapNode>,
        new: &Arc<SHAMapNode>,
        diffs: &mut Vec<DiffEntry>,
        old_store: Option<&Arc<dyn NodeStore>>,
        new_store: Option<&Arc<dyn NodeStore>>,
        old_leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        new_leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) {
        // Same hash means identical subtree
        if old.hash() == new.hash() && !old.hash().is_zero() {
            return;
        }

        match (old.as_ref(), new.as_ref()) {
            (SHAMapNode::Inner(old_inner), SHAMapNode::Inner(new_inner)) => {
                Self::diff_inner_nodes(
                    old_inner,
                    new_inner,
                    diffs,
                    old_store,
                    new_store,
                    old_leaf_ctor,
                    new_leaf_ctor,
                );
            }
            (SHAMapNode::Leaf(old_leaf), SHAMapNode::Leaf(new_leaf)) => {
                if old_leaf.key() == new_leaf.key() {
                    if old_leaf.data() != new_leaf.data() {
                        diffs.push(DiffEntry::Modified {
                            key: *old_leaf.key(),
                        });
                    }
                } else {
                    diffs.push(DiffEntry::Removed {
                        key: *old_leaf.key(),
                    });
                    diffs.push(DiffEntry::Added {
                        key: *new_leaf.key(),
                    });
                }
            }
            (SHAMapNode::Leaf(old_leaf), SHAMapNode::Inner(new_inner)) => {
                diffs.push(DiffEntry::Removed {
                    key: *old_leaf.key(),
                });
                Self::collect_all_leaves_inner(new_inner, diffs, true, new_store, new_leaf_ctor);
            }
            (SHAMapNode::Inner(old_inner), SHAMapNode::Leaf(new_leaf)) => {
                Self::collect_all_leaves_inner(old_inner, diffs, false, old_store, old_leaf_ctor);
                diffs.push(DiffEntry::Added {
                    key: *new_leaf.key(),
                });
            }
        }
    }

    fn diff_inner_nodes(
        old: &InnerNode,
        new: &InnerNode,
        diffs: &mut Vec<DiffEntry>,
        old_store: Option<&Arc<dyn NodeStore>>,
        new_store: Option<&Arc<dyn NodeStore>>,
        old_leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        new_leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) {
        for branch in 0..16u8 {
            let old_child = old
                .child_with_store(branch, old_store, old_leaf_ctor)
                .ok()
                .flatten();
            let new_child = new
                .child_with_store(branch, new_store, new_leaf_ctor)
                .ok()
                .flatten();

            match (old_child, new_child) {
                (None, None) => {}
                (Some(old_c), None) => {
                    Self::collect_all_leaves(old_c, diffs, false, old_store, old_leaf_ctor);
                }
                (None, Some(new_c)) => {
                    Self::collect_all_leaves(new_c, diffs, true, new_store, new_leaf_ctor);
                }
                (Some(old_c), Some(new_c)) => {
                    if old_c.hash() != new_c.hash() || old_c.hash().is_zero() {
                        Self::diff_nodes(
                            old_c,
                            new_c,
                            diffs,
                            old_store,
                            new_store,
                            old_leaf_ctor,
                            new_leaf_ctor,
                        );
                    }
                }
            }
        }
    }

    fn collect_all_leaves(
        node: &Arc<SHAMapNode>,
        diffs: &mut Vec<DiffEntry>,
        added: bool,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) {
        match node.as_ref() {
            SHAMapNode::Leaf(leaf) => {
                if added {
                    diffs.push(DiffEntry::Added { key: *leaf.key() });
                } else {
                    diffs.push(DiffEntry::Removed { key: *leaf.key() });
                }
            }
            SHAMapNode::Inner(inner) => {
                Self::collect_all_leaves_inner(inner, diffs, added, store, leaf_ctor);
            }
        }
    }

    fn collect_all_leaves_inner(
        inner: &InnerNode,
        diffs: &mut Vec<DiffEntry>,
        added: bool,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) {
        let mut mask = inner.branch_mask();
        while mask != 0 {
            let branch = mask.trailing_zeros() as u8;
            if let Ok(Some(child)) = inner.child_with_store(branch, store, leaf_ctor) {
                Self::collect_all_leaves(child, diffs, added, store, leaf_ctor);
            }
            mask &= mask - 1;
        }
    }

    /// Reconstruct a SHAMap from downloaded leaf (key, data) pairs.
    ///
    /// Creates a mutable account state map and inserts all provided nodes.
    /// Each key must be exactly 32 bytes.
    pub fn from_leaf_nodes(nodes: &[(Vec<u8>, Vec<u8>)]) -> Result<SHAMap, SHAMapError> {
        let mut map = SHAMap::account_state();
        for (key_bytes, data_bytes) in nodes {
            if key_bytes.len() != 32 {
                return Err(SHAMapError::InvalidKeyLength(key_bytes.len()));
            }
            let key = Hash256::new(key_bytes.as_slice().try_into().unwrap());
            map.put(key, data_bytes.clone())?;
        }
        Ok(map)
    }

    /// Verify that the root hash matches an expected value.
    pub fn verify_root_hash(&mut self, expected: &Hash256) -> bool {
        self.root_hash() == *expected
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

    fn find_leaf<'a>(
        node: &'a InnerNode,
        key: &Hash256,
        depth: u8,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Option<&'a LeafNode> {
        let branch = select_branch(key, depth);
        let child = node.child_with_store(branch, store, leaf_ctor).ok()??;
        match child.as_ref() {
            SHAMapNode::Leaf(leaf) => {
                if leaf.key() == key {
                    Some(leaf)
                } else {
                    None
                }
            }
            SHAMapNode::Inner(inner) => Self::find_leaf(inner, key, depth + 1, store, leaf_ctor),
        }
    }

    fn insert_leaf(
        node: &mut InnerNode,
        key: Hash256,
        leaf: LeafNode,
        depth: u8,
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, SHAMapNode::Leaf(leaf));
            return Ok(());
        }

        node.ensure_loaded(branch, store, leaf_ctor)?;
        match node.take_child(branch) {
            Some(arc) => {
                // Try to unwrap, otherwise clone
                match Arc::try_unwrap(arc) {
                    Ok(owned) => {
                        Self::insert_into_existing(
                            node, branch, owned, key, leaf, depth, store, leaf_ctor,
                        )
                    }
                    Err(shared) => {
                        let owned = (*shared).clone();
                        Self::insert_into_existing(
                            node, branch, owned, key, leaf, depth, store, leaf_ctor,
                        )
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
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
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
                Self::insert_leaf(&mut inner, key, leaf, depth + 1, store, leaf_ctor)?;
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
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(&key, depth);

        if node.is_empty_branch(branch) {
            node.set_child(branch, SHAMapNode::Leaf(leaf));
            return Ok(());
        }

        node.ensure_loaded(branch, store, leaf_ctor)?;
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
                        Self::put_leaf(&mut inner, key, leaf, depth + 1, store, leaf_ctor)?;
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
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<Vec<u8>, SHAMapError> {
        let branch = select_branch(key, depth);

        if node.is_empty_branch(branch) {
            return Err(SHAMapError::NotFound);
        }

        node.ensure_loaded(branch, store, leaf_ctor)?;
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
                        let data =
                            Self::delete_from(&mut inner, key, depth + 1, store, leaf_ctor)?;
                        if inner.branch_count() == 0 {
                            // Empty inner, don't re-insert
                        } else if let Some(single) = inner.single_branch() {
                            // Need to ensure the single child is loaded before taking
                            inner.ensure_loaded(single, store, leaf_ctor)?;
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
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> Result<(), SHAMapError> {
        let branch = select_branch(key, depth);

        if node.is_empty_branch(branch) {
            return Err(SHAMapError::NotFound);
        }

        node.ensure_loaded(branch, store, leaf_ctor)?;
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
                        let result =
                            Self::update_in(&mut inner, key, data, depth + 1, store, leaf_ctor);
                        node.set_child(branch, SHAMapNode::Inner(inner));
                        result
                    }
                }
            }
            None => Err(SHAMapError::NotFound),
        }
    }

    fn visit(
        node: &InnerNode,
        f: &mut impl FnMut(&Hash256, &[u8]),
        store: Option<&Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) {
        let mut mask = node.branch_mask();
        while mask != 0 {
            let branch = mask.trailing_zeros() as u8;
            if let Ok(Some(child)) = node.child_with_store(branch, store, leaf_ctor) {
                match child.as_ref() {
                    SHAMapNode::Leaf(leaf) => f(leaf.key(), leaf.data()),
                    SHAMapNode::Inner(inner) => Self::visit(inner, f, store, leaf_ctor),
                }
            }
            mask &= mask - 1;
        }
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
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");

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
        let k1 = make_key("A100000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("A200000000000000000000000000000000000000000000000000000000000000");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        assert_eq!(map.get(&k1), Some(&[1][..]));
        assert_eq!(map.get(&k2), Some(&[2][..]));
    }

    #[test]
    fn keys_sharing_long_prefix() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("ABCDEF0000000000000000000000000000000000000000000000000000000001");
        let k2 = make_key("ABCDEF0000000000000000000000000000000000000000000000000000000002");

        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();

        assert_eq!(map.get(&k1), Some(&[1][..]));
        assert_eq!(map.get(&k2), Some(&[2][..]));
    }

    #[test]
    fn root_hash_changes_on_modification() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");

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
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");

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
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");

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

        let key = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        assert_eq!(map.put(key, vec![1]), Err(SHAMapError::Immutable));
        assert_eq!(map.delete(&key), Err(SHAMapError::Immutable));
    }

    #[test]
    fn snapshot_is_independent() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
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
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");

        map.put(key, vec![1]).unwrap();
        map.update(key, vec![9, 9, 9]).unwrap();
        assert_eq!(map.get(&key), Some(&[9, 9, 9][..]));
    }

    #[test]
    fn update_nonexistent_fails() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(map.update(key, vec![1]), Err(SHAMapError::NotFound));
    }

    #[test]
    fn delete_with_consolidation() {
        let mut map = SHAMap::account_state();
        let k1 = make_key("A100000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("A200000000000000000000000000000000000000000000000000000000000000");

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
    fn diff_identical_maps() {
        let mut a = SHAMap::account_state();
        let k = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        a.put(k, vec![1]).unwrap();
        a.root_hash();

        let b = a.snapshot();
        let diffs = a.find_difference(&b);
        assert!(diffs.is_empty());
    }

    #[test]
    fn diff_added_key() {
        let mut a = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        a.put(k1, vec![1]).unwrap();
        a.root_hash();

        let mut b = a.mutable_copy();
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        b.put(k2, vec![2]).unwrap();
        b.root_hash();

        let diffs = a.find_difference(&b);
        assert!(diffs.contains(&DiffEntry::Added { key: k2 }));
    }

    #[test]
    fn diff_removed_key() {
        let mut a = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        a.put(k1, vec![1]).unwrap();
        a.put(k2, vec![2]).unwrap();
        a.root_hash();

        let mut b = a.mutable_copy();
        b.delete(&k2).unwrap();
        b.root_hash();

        let diffs = a.find_difference(&b);
        assert!(diffs.contains(&DiffEntry::Removed { key: k2 }));
    }

    #[test]
    fn diff_modified_key() {
        let mut a = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        a.put(k1, vec![1]).unwrap();
        a.root_hash();

        let mut b = a.mutable_copy();
        b.put(k1, vec![99]).unwrap();
        b.root_hash();

        let diffs = a.find_difference(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs.contains(&DiffEntry::Modified { key: k1 }));
    }

    #[test]
    fn diff_empty_to_populated() {
        let a = SHAMap::account_state();
        let mut b = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        b.put(k1, vec![1]).unwrap();
        b.put(k2, vec![2]).unwrap();

        let diffs = a.find_difference(&b);
        assert_eq!(diffs.len(), 2);
        assert!(diffs.contains(&DiffEntry::Added { key: k1 }));
        assert!(diffs.contains(&DiffEntry::Added { key: k2 }));
    }

    #[test]
    fn diff_populated_to_empty() {
        let mut a = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        a.put(k1, vec![1]).unwrap();

        let b = SHAMap::account_state();
        let diffs = a.find_difference(&b);
        assert_eq!(diffs.len(), 1);
        assert!(diffs.contains(&DiffEntry::Removed { key: k1 }));
    }

    #[test]
    fn diff_complex_changes() {
        let mut a = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");
        a.put(k1, vec![1]).unwrap();
        a.put(k2, vec![2]).unwrap();
        a.put(k3, vec![3]).unwrap();
        a.root_hash();

        let mut b = a.mutable_copy();
        b.delete(&k1).unwrap(); // removed
        b.put(k2, vec![22]).unwrap(); // modified
        // k3 unchanged
        let k4 = make_key("4000000000000000000000000000000000000000000000000000000000000000");
        b.put(k4, vec![4]).unwrap(); // added
        b.root_hash();

        let diffs = a.find_difference(&b);
        assert!(diffs.contains(&DiffEntry::Removed { key: k1 }));
        assert!(diffs.contains(&DiffEntry::Modified { key: k2 }));
        assert!(diffs.contains(&DiffEntry::Added { key: k4 }));
        // k3 should NOT appear in diffs
        assert!(!diffs.iter().any(|d| match d {
            DiffEntry::Added { key } | DiffEntry::Removed { key } | DiffEntry::Modified { key } => *key == k3,
        }));
    }

    #[test]
    fn from_leaf_nodes_round_trip() {
        let mut original = SHAMap::account_state();
        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");
        original.put(k1, vec![1, 2]).unwrap();
        original.put(k2, vec![3, 4]).unwrap();
        original.put(k3, vec![5, 6]).unwrap();
        let original_hash = original.root_hash();

        // Serialize leaves
        let mut leaves = Vec::new();
        original.for_each(&mut |key, data| {
            leaves.push((key.as_bytes().to_vec(), data.to_vec()));
        });

        // Reconstruct
        let mut reconstructed = SHAMap::from_leaf_nodes(&leaves).unwrap();
        assert_eq!(reconstructed.root_hash(), original_hash);
        assert_eq!(reconstructed.get(&k1), Some(&[1, 2][..]));
        assert_eq!(reconstructed.get(&k2), Some(&[3, 4][..]));
        assert_eq!(reconstructed.get(&k3), Some(&[5, 6][..]));
    }

    #[test]
    fn from_leaf_nodes_invalid_key_length() {
        let nodes = vec![(vec![1, 2, 3], vec![4, 5, 6])]; // key is 3 bytes, not 32
        let result = SHAMap::from_leaf_nodes(&nodes);
        assert!(matches!(result, Err(SHAMapError::InvalidKeyLength(3))));
    }

    #[test]
    fn from_leaf_nodes_empty() {
        let mut map = SHAMap::from_leaf_nodes(&[]).unwrap();
        assert!(map.is_empty());
        assert_eq!(map.root_hash(), Hash256::ZERO);
    }

    #[test]
    fn verify_root_hash_pass() {
        let mut map = SHAMap::account_state();
        let k = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        map.put(k, vec![1]).unwrap();
        let hash = map.root_hash();
        assert!(map.verify_root_hash(&hash));
    }

    #[test]
    fn verify_root_hash_fail() {
        let mut map = SHAMap::account_state();
        let k = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        map.put(k, vec![1]).unwrap();
        let wrong = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        assert!(!map.verify_root_hash(&wrong));
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

    #[test]
    fn from_root_hash_round_trip() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());

        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        let k3 = make_key("3000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();
        map.put(k3, vec![3]).unwrap();

        let root_hash = map.flush().unwrap();

        // Reconstruct from root hash
        let loaded =
            SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
        assert_eq!(loaded.get(&k1), Some(&[1][..]));
        assert_eq!(loaded.get(&k2), Some(&[2][..]));
        assert_eq!(loaded.get(&k3), Some(&[3][..]));
    }

    #[test]
    fn from_root_hash_iterate() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());

        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![10]).unwrap();
        map.put(k2, vec![20]).unwrap();

        let root_hash = map.flush().unwrap();

        let loaded =
            SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
        let items: Vec<_> = loaded.iter().collect();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn from_root_hash_for_each() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());

        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![42]).unwrap();

        let root_hash = map.flush().unwrap();

        let loaded =
            SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
        let mut visited = Vec::new();
        loaded.for_each(&mut |key, data| {
            visited.push((*key, data.to_vec()));
        });
        assert_eq!(visited.len(), 1);
        assert_eq!(visited[0].0, k1);
        assert_eq!(visited[0].1, vec![42]);
    }

    #[test]
    fn incremental_flush() {
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = SHAMap::account_state_with_store(store.clone());

        let k1 = make_key("1000000000000000000000000000000000000000000000000000000000000000");
        let k2 = make_key("2000000000000000000000000000000000000000000000000000000000000000");
        map.put(k1, vec![1]).unwrap();
        map.put(k2, vec![2]).unwrap();
        let root_hash_1 = map.flush().unwrap();

        // Load from store, modify one leaf
        let mut loaded =
            SHAMap::from_root_hash(root_hash_1, LeafNode::account_state, store.clone())
                .unwrap();
        loaded.put(k1, vec![99]).unwrap();
        let root_hash_2 = loaded.flush().unwrap();

        assert_ne!(root_hash_1, root_hash_2);

        // Verify the modified tree
        let reloaded =
            SHAMap::from_root_hash(root_hash_2, LeafNode::account_state, store).unwrap();
        assert_eq!(reloaded.get(&k1), Some(&[99][..]));
        assert_eq!(reloaded.get(&k2), Some(&[2][..]));
    }

    #[test]
    fn from_root_hash_missing_root() {
        let store = Arc::new(InMemoryNodeStore::new());
        let missing = make_key("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF");
        let result = SHAMap::from_root_hash(missing, LeafNode::account_state, store);
        assert!(result.is_err());
    }

    use crate::leaf_node::LeafNode;
}
