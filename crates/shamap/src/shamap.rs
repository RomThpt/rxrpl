use std::sync::Arc;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::inner_node::InnerNode;
use crate::iterator::{SHAMapIter, SHAMapRefIter};
use crate::leaf_node::LeafNode;
use crate::node::{SHAMapNode, SHAMapState};
use crate::node_id::{NodeId, select_branch};
use crate::node_store::NodeStore;

/// A node missing from the SHAMap during incremental sync.
#[derive(Clone, Debug)]
pub struct MissingNode {
    /// Content hash for store operations.
    pub hash: Hash256,
    /// SHAMapNodeID for wire protocol (33 bytes: 32 path + 1 depth).
    pub node_id: NodeId,
}

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
            SHAMapNode::Inner(inner) => Self::visit(inner, f, self.store.as_ref(), self.leaf_ctor),
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
                continue; // Skip entries with invalid key length.
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
                    Ok(owned) => Self::insert_into_existing(
                        node, branch, owned, key, leaf, depth, store, leaf_ctor,
                    ),
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
                        let data = Self::delete_from(&mut inner, key, depth + 1, store, leaf_ctor)?;
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

    // --- Pruning helpers ---

    /// Collect hashes of all nodes (inner + leaf) reachable from this tree.
    ///
    /// Walks the tree through the backing store, loading nodes lazily.
    /// Returns the set of every `Hash256` that forms part of this map.
    /// Used by ledger pruning to determine which nodes are still referenced.
    pub fn collect_all_node_hashes(&self) -> Vec<Hash256> {
        let mut hashes = Vec::new();
        Self::walk_node_hashes(&self.root, &self.store, self.leaf_ctor, &mut hashes);
        hashes
    }

    fn walk_node_hashes(
        node: &Arc<SHAMapNode>,
        store: &Option<Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        out: &mut Vec<Hash256>,
    ) {
        match node.as_ref() {
            SHAMapNode::Inner(inner) => {
                let hash = inner.hash();
                if !hash.is_zero() {
                    out.push(hash);
                }
                let mut mask = inner.branch_mask();
                while mask != 0 {
                    let branch = mask.trailing_zeros() as u8;
                    if let Ok(Some(child)) =
                        inner.child_with_store(branch, store.as_ref(), leaf_ctor)
                    {
                        Self::walk_node_hashes(child, store, leaf_ctor, out);
                    } else {
                        // Unresolvable branch: record the hash if known
                        let ch = inner.child_hash(branch);
                        if !ch.is_zero() {
                            out.push(ch);
                        }
                    }
                    mask &= mask - 1;
                }
            }
            SHAMapNode::Leaf(leaf) => {
                out.push(leaf.hash());
            }
        }
    }

    // --- Delta sync methods ---

    /// Look up a SHAMap node by its `NodeId` (path + depth).
    ///
    /// Walks from the root following `select_branch(node_id.id(), d)` for
    /// `d in 0..node_id.depth()`. Returns the node found at that position
    /// (loaded from the backing store if not in memory) along with its
    /// content hash and serialized bytes.
    ///
    /// Wire format (matching rippled's `serializeShallow`):
    /// - Inner node: 16 × 32 = 512 bytes (concatenated child hashes)
    /// - Leaf node: key(32) || data
    ///
    /// Used by `handle_get_ledger` to serve rippled-style 33-byte NodeId
    /// requests (path + depth). Returns `None` if any branch on the path
    /// is empty or unloadable from the store.
    pub fn node_at(&self, node_id: NodeId) -> Option<(Hash256, Vec<u8>)> {
        let target_depth = node_id.depth();
        let key = *node_id.id();
        let mut current: &Arc<SHAMapNode> = &self.root;

        for d in 0..target_depth {
            let inner = match current.as_ref() {
                SHAMapNode::Inner(i) => i,
                SHAMapNode::Leaf(_) => return None,
            };
            let branch = select_branch(&key, d);
            current = inner
                .child_with_store(branch, self.store.as_ref(), self.leaf_ctor)
                .ok()
                .flatten()?;
        }

        Some(serialize_node_shallow(current))
    }

    /// Compute hashes of missing nodes needed to complete the tree toward a target root.
    ///
    /// Walks the tree comparing known inner node hashes against what is in the
    /// backing store. A branch is "missing" if `is_branch` is set but the child
    /// is neither loaded in memory nor fetchable from the store.
    ///
    /// If `self` has no root (empty tree), returns the `target_root_hash` itself
    /// so the caller can request the root node first.
    ///
    /// Returns up to `max_count` hashes of missing nodes.
    pub fn missing_nodes(&self, target_root_hash: Hash256, max_count: usize) -> Vec<MissingNode> {
        if max_count == 0 {
            return Vec::new();
        }

        // If our tree is empty, we need the target root itself.
        if self.is_empty() {
            return vec![MissingNode {
                hash: target_root_hash,
                node_id: NodeId::ROOT,
            }];
        }

        let mut missing = Vec::new();
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => {
                Self::collect_missing(
                    inner,
                    &self.store,
                    self.leaf_ctor,
                    max_count,
                    0,
                    Hash256::ZERO,
                    &mut missing,
                );
            }
            SHAMapNode::Leaf(_) => {}
        }
        missing
    }

    /// Recursively collect nodes that are referenced but not available,
    /// tracking the SHAMapNodeID (path + depth) for the wire protocol.
    fn collect_missing(
        inner: &InnerNode,
        store: &Option<Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        max_count: usize,
        depth: u8,
        path: Hash256,
        out: &mut Vec<MissingNode>,
    ) {
        let mut mask = inner.branch_mask();
        while mask != 0 && out.len() < max_count {
            let branch = mask.trailing_zeros() as u8;

            // Build child path: set the nibble at `depth` to `branch`.
            let mut child_path = *path.as_bytes();
            let byte_idx = (depth / 2) as usize;
            if depth & 1 == 0 {
                child_path[byte_idx] = (child_path[byte_idx] & 0x0F) | (branch << 4);
            } else {
                child_path[byte_idx] = (child_path[byte_idx] & 0xF0) | branch;
            }
            let child_key = Hash256::new(child_path);
            let child_node_id = NodeId::new(depth + 1, &child_key);

            // Try to get the child: first check if loaded, then try store.
            if inner.child(branch).is_some() {
                if let Some(child) = inner.child(branch) {
                    if let SHAMapNode::Inner(child_inner) = child.as_ref() {
                        Self::collect_missing(
                            child_inner,
                            store,
                            leaf_ctor,
                            max_count,
                            depth + 1,
                            child_key,
                            out,
                        );
                    }
                }
            } else {
                let child_hash = inner.child_hash(branch);
                let available = store
                    .as_ref()
                    .and_then(|s| s.fetch(&child_hash).ok())
                    .flatten()
                    .is_some();
                if !available {
                    out.push(MissingNode {
                        hash: child_hash,
                        node_id: child_node_id,
                    });
                } else {
                    if let Some(s) = store.as_ref() {
                        if let Ok(Some(data)) = s.fetch(&child_hash) {
                            if let Ok(node) =
                                crate::node_store::deserialize_node(&data, &child_hash, leaf_ctor)
                            {
                                if let SHAMapNode::Inner(child_inner) = &node {
                                    Self::collect_missing(
                                        child_inner,
                                        store,
                                        leaf_ctor,
                                        max_count,
                                        depth + 1,
                                        child_key,
                                        out,
                                    );
                                }
                            }
                        }
                    }
                }
            }

            mask &= mask - 1;
        }
    }

    /// Insert a raw node (inner or leaf) fetched from a peer into the backing store.
    ///
    /// The node data is stored via `store.store_batch()`. Lazy loading will pick
    /// it up when the tree is traversed. Returns `Ok(true)` if the node was new
    /// (not already present), `Ok(false)` if it was already in the store.
    pub fn add_raw_node(&mut self, hash: Hash256, data: Vec<u8>) -> Result<bool, SHAMapError> {
        let store = self.store.as_ref().ok_or(SHAMapError::MissingStore)?;

        // Check if already present.
        if let Some(_existing) = store.fetch(&hash)? {
            return Ok(false);
        }

        store.store_batch(&[(&hash, &data)])?;
        Ok(true)
    }

    /// Check if the tree is complete (all branches can be resolved).
    ///
    /// Walks the tree and returns `true` if every branch's child is either
    /// loaded in memory or fetchable from the backing store. Returns `false`
    /// if any branch cannot be resolved (missing store or missing node).
    pub fn is_complete(&self) -> bool {
        match self.root.as_ref() {
            SHAMapNode::Inner(inner) => Self::check_complete(inner, &self.store, self.leaf_ctor),
            SHAMapNode::Leaf(_) => true,
        }
    }

    /// Recursively check completeness of an inner node.
    fn check_complete(
        inner: &InnerNode,
        store: &Option<Arc<dyn NodeStore>>,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
    ) -> bool {
        let mut mask = inner.branch_mask();
        while mask != 0 {
            let branch = mask.trailing_zeros() as u8;

            match inner.child_with_store(branch, store.as_ref(), leaf_ctor) {
                Ok(Some(child)) => {
                    if let SHAMapNode::Inner(child_inner) = child.as_ref() {
                        if !Self::check_complete(child_inner, store, leaf_ctor) {
                            return false;
                        }
                    }
                }
                _ => return false,
            }

            mask &= mask - 1;
        }
        true
    }

    /// Create a SHAMap in syncing state from a root hash stored in the backing
    /// store. Unlike `from_root_hash`, this does not fail if the root is not
    /// yet in the store -- it creates an empty tree that will be populated
    /// incrementally via `add_raw_node`.
    pub fn syncing_with_store(
        root_hash: Hash256,
        leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
        store: Arc<dyn NodeStore>,
    ) -> Self {
        // Try to load the root if already available.
        let root = store
            .fetch(&root_hash)
            .ok()
            .flatten()
            .and_then(|bytes| {
                crate::node_store::deserialize_node(&bytes, &root_hash, leaf_ctor).ok()
            })
            .map(|node| Arc::new(node))
            .unwrap_or_else(|| Arc::new(SHAMapNode::inner(InnerNode::new())));

        Self {
            root,
            state: SHAMapState::Syncing,
            leaf_ctor,
            store: Some(store),
        }
    }

    /// Reload the root node from the store after new nodes have been added.
    ///
    /// Used during incremental sync to pick up a newly stored root node.
    pub fn reload_root(&mut self, root_hash: Hash256) -> Result<(), SHAMapError> {
        let store = self.store.as_ref().ok_or(SHAMapError::MissingStore)?;
        if let Some(bytes) = store.fetch(&root_hash)? {
            let node = crate::node_store::deserialize_node(&bytes, &root_hash, self.leaf_ctor)?;
            self.root = Arc::new(node);
        }
        Ok(())
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

/// Serialize a SHAMap node to its on-the-wire shallow form.
///
/// Mirrors `collect_dirty_nodes`: inner = 16x32 child hashes, leaf = key||data.
/// The trailing depth byte expected by rippled's TMLedgerNode wire format is
/// NOT appended here; that is the caller's responsibility (since depth comes
/// from the NodeId being requested, not the node itself).
fn serialize_node_shallow(node: &Arc<SHAMapNode>) -> (Hash256, Vec<u8>) {
    match node.as_ref() {
        SHAMapNode::Inner(inner) => {
            let mut data = Vec::with_capacity(16 * 32);
            for i in 0..16u8 {
                data.extend_from_slice(inner.child_hash(i).as_bytes());
            }
            (inner.hash(), data)
        }
        SHAMapNode::Leaf(leaf) => {
            let mut data = Vec::with_capacity(32 + leaf.data().len());
            data.extend_from_slice(leaf.key().as_bytes());
            data.extend_from_slice(leaf.data());
            (leaf.hash(), data)
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
    fn node_at_root_returns_root_inner() {
        let mut map = SHAMap::account_state();
        for i in 0u8..16 {
            let mut k = [0u8; 32];
            k[0] = i << 4;
            map.put(Hash256::new(k), vec![i, i, i]).unwrap();
        }
        map.flush().unwrap();

        let (hash, bytes) = map.node_at(NodeId::ROOT).expect("root must exist");
        assert_eq!(hash, map.root_hash());
        assert_eq!(bytes.len(), 16 * 32, "inner root serializes as 16 hashes");
    }

    #[test]
    fn node_at_leaf_round_trip() {
        let mut map = SHAMap::account_state();
        let key = make_key("ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789");
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        map.put(key, data.clone()).unwrap();
        map.flush().unwrap();

        // Find the leaf via node_at by walking the same path.
        // For this single-leaf tree the leaf sits as a direct child of the root.
        let branch = select_branch(&key, 0);
        let _ = branch; // depth=1 lookup
        let leaf_node_id = NodeId::new(1, &key);
        let (_h, bytes) = map.node_at(leaf_node_id).expect("leaf must exist");
        assert_eq!(&bytes[..32], key.as_bytes(), "leaf bytes start with key");
        assert_eq!(&bytes[32..], &data[..], "then leaf data");
    }

    #[test]
    fn node_at_missing_branch_returns_none() {
        let mut map = SHAMap::account_state();
        let key = make_key("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
        map.put(key, vec![1]).unwrap();
        map.flush().unwrap();

        // Try to walk a path that doesn't exist.
        let other = make_key("FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF");
        let missing = NodeId::new(5, &other);
        assert!(map.node_at(missing).is_none());
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
            DiffEntry::Added { key } | DiffEntry::Removed { key } | DiffEntry::Modified { key } =>
                *key == k3,
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
    fn from_leaf_nodes_invalid_key_length_skipped() {
        let nodes = vec![(vec![1, 2, 3], vec![4, 5, 6])]; // key is 3 bytes, not 32
        let mut result = SHAMap::from_leaf_nodes(&nodes).unwrap();
        assert!(result.is_empty()); // Invalid entries are skipped.
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
        let loaded = SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
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

        let loaded = SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
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

        let loaded = SHAMap::from_root_hash(root_hash, LeafNode::account_state, store).unwrap();
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
            SHAMap::from_root_hash(root_hash_1, LeafNode::account_state, store.clone()).unwrap();
        loaded.put(k1, vec![99]).unwrap();
        let root_hash_2 = loaded.flush().unwrap();

        assert_ne!(root_hash_1, root_hash_2);

        // Verify the modified tree
        let reloaded = SHAMap::from_root_hash(root_hash_2, LeafNode::account_state, store).unwrap();
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
