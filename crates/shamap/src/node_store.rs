use std::collections::HashMap;
use std::sync::RwLock;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::inner_node::InnerNode;
use crate::leaf_node::LeafNode;
use crate::node::SHAMapNode;
use crate::node_id::BRANCH_FACTOR;

/// Store-record type tags. A leaf serialized as `key(32) || data` collides in
/// length with an inner node (16 x 32 = 512 bytes) exactly when the leaf value
/// is 480 bytes, so the record type must be explicit rather than inferred from
/// length. The tag does not affect the node hash (computed from node content,
/// not the store record), so hashes stay rippled-compatible.
pub const STORE_TAG_INNER: u8 = 0x00;
pub const STORE_TAG_LEAF: u8 = 0x01;

/// Serialize an inner node's 16 child hashes into a store record.
pub fn serialize_inner(child_hashes: [Hash256; BRANCH_FACTOR]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + BRANCH_FACTOR * 32);
    out.push(STORE_TAG_INNER);
    for h in &child_hashes {
        out.extend_from_slice(h.as_bytes());
    }
    out
}

/// Serialize a leaf node (`key || data`) into a store record.
pub fn serialize_leaf(key: &Hash256, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 32 + data.len());
    out.push(STORE_TAG_LEAF);
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(data);
    out
}

/// Deserialize a node from its stored byte representation.
///
/// The first byte is a type tag ([`STORE_TAG_INNER`] / [`STORE_TAG_LEAF`]);
/// inner = tag || 16 x 32-byte child hashes, leaf = tag || key(32) || data.
pub fn deserialize_node(
    bytes: &[u8],
    hash: &Hash256,
    leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
) -> Result<SHAMapNode, SHAMapError> {
    match bytes.split_first() {
        Some((&STORE_TAG_INNER, body)) if body.len() == BRANCH_FACTOR * 32 => {
            let mut inner = InnerNode::new();
            for i in 0..BRANCH_FACTOR {
                let start = i * 32;
                let child_hash = Hash256::new(body[start..start + 32].try_into().unwrap());
                if !child_hash.is_zero() {
                    inner.set_child_hash(i as u8, child_hash);
                }
            }
            inner.set_cached_hash(*hash);
            Ok(SHAMapNode::inner(inner))
        }
        Some((&STORE_TAG_LEAF, body)) if body.len() >= 32 => {
            let key = Hash256::new(body[..32].try_into().unwrap());
            let data = body[32..].to_vec();
            Ok(SHAMapNode::Leaf(leaf_ctor(key, data)))
        }
        _ => Err(SHAMapError::DeserializeError),
    }
}

/// Pluggable storage backend for SHAMap nodes.
///
/// Allows future implementation of persistent backends (RocksDB, etc.).
pub trait NodeStore: Send + Sync {
    fn fetch(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, SHAMapError>;
    fn store_batch(&self, entries: &[(&Hash256, &[u8])]) -> Result<(), SHAMapError>;

    /// Delete a batch of nodes by hash. Used by ledger history pruning.
    /// The default implementation is a no-op for backward compatibility.
    fn delete_batch(&self, _hashes: &[Hash256]) -> Result<(), SHAMapError> {
        Ok(())
    }

    /// Number of nodes currently held. Default 0 for backends that do not track
    /// it; used to observe catchup progress (real store growth, as opposed to a
    /// single sync's added count).
    fn len(&self) -> usize {
        0
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// In-memory node store backed by a HashMap.
pub struct InMemoryNodeStore {
    data: RwLock<HashMap<Hash256, Vec<u8>>>,
}

impl InMemoryNodeStore {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryNodeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeStore for InMemoryNodeStore {
    fn fetch(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, SHAMapError> {
        let data = self.data.read().map_err(|_| SHAMapError::InvalidNode)?;
        Ok(data.get(hash).cloned())
    }

    fn store_batch(&self, entries: &[(&Hash256, &[u8])]) -> Result<(), SHAMapError> {
        let mut data = self.data.write().map_err(|_| SHAMapError::InvalidNode)?;
        for (hash, bytes) in entries {
            data.insert(**hash, bytes.to_vec());
        }
        Ok(())
    }

    fn delete_batch(&self, hashes: &[Hash256]) -> Result<(), SHAMapError> {
        let mut data = self.data.write().map_err(|_| SHAMapError::InvalidNode)?;
        for hash in hashes {
            data.remove(hash);
        }
        Ok(())
    }

    fn len(&self) -> usize {
        self.data.read().map(|d| d.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner_node::InnerNode;
    use crate::leaf_node::LeafNode;
    use crate::node::SHAMapNode;
    use crate::node_id::BRANCH_FACTOR;

    #[test]
    fn fetch_missing() {
        let store = InMemoryNodeStore::new();
        let hash = Hash256::new([0xAA; 32]);
        assert_eq!(store.fetch(&hash).unwrap(), None);
    }

    #[test]
    fn store_and_fetch() {
        let store = InMemoryNodeStore::new();
        let hash = Hash256::new([0xBB; 32]);
        let data = vec![1, 2, 3, 4];
        store.store_batch(&[(&hash, &data)]).unwrap();
        assert_eq!(store.fetch(&hash).unwrap(), Some(data));
    }

    #[test]
    fn store_batch_multiple() {
        let store = InMemoryNodeStore::new();
        let h1 = Hash256::new([0x01; 32]);
        let h2 = Hash256::new([0x02; 32]);
        let d1 = vec![10];
        let d2 = vec![20];
        store.store_batch(&[(&h1, &d1), (&h2, &d2)]).unwrap();
        assert_eq!(store.fetch(&h1).unwrap(), Some(d1));
        assert_eq!(store.fetch(&h2).unwrap(), Some(d2));
    }

    #[test]
    fn overwrite() {
        let store = InMemoryNodeStore::new();
        let hash = Hash256::new([0xCC; 32]);
        store.store_batch(&[(&hash, &[1, 2])]).unwrap();
        store.store_batch(&[(&hash, &[3, 4])]).unwrap();
        assert_eq!(store.fetch(&hash).unwrap(), Some(vec![3, 4]));
    }

    #[test]
    fn deserialize_inner_node_round_trip() {
        // Build an inner node with two child hashes
        let mut inner = InnerNode::new();
        let h1 = Hash256::new([0x11; 32]);
        let h2 = Hash256::new([0x22; 32]);
        inner.set_child_hash(0, h1);
        inner.set_child_hash(5, h2);
        inner.update_hash();
        let hash = inner.hash();

        let mut child_hashes = [Hash256::ZERO; BRANCH_FACTOR];
        for (i, slot) in child_hashes.iter_mut().enumerate() {
            *slot = inner.child_hash(i as u8);
        }
        let data = super::serialize_inner(child_hashes);

        let node = super::deserialize_node(&data, &hash, LeafNode::account_state).unwrap();
        match &node {
            SHAMapNode::Inner(deserialized) => {
                assert_eq!(deserialized.child_hash(0), h1);
                assert_eq!(deserialized.child_hash(5), h2);
                assert!(deserialized.is_empty_branch(1));
                assert_eq!(deserialized.hash(), hash);
            }
            _ => panic!("expected inner node"),
        }
    }

    #[test]
    fn deserialize_leaf_node_round_trip() {
        let key = Hash256::new([0xAA; 32]);
        let leaf_data = vec![1, 2, 3, 4, 5];
        let leaf = LeafNode::account_state(key, leaf_data.clone());
        let leaf_hash = leaf.hash();

        let bytes = super::serialize_leaf(&key, &leaf_data);

        let node = super::deserialize_node(&bytes, &leaf_hash, LeafNode::account_state).unwrap();
        match &node {
            SHAMapNode::Leaf(deserialized) => {
                assert_eq!(*deserialized.key(), key);
                assert_eq!(deserialized.data(), &leaf_data[..]);
            }
            _ => panic!("expected leaf node"),
        }
    }

    #[test]
    fn leaf_with_480_byte_value_is_not_confused_with_inner() {
        // A leaf value of exactly 480 bytes makes key(32)||data == 512 bytes,
        // the length of an inner node's 16 child hashes. The type tag must keep
        // it decoded as a leaf, not a garbage inner node.
        let key = Hash256::new([0xCD; 32]);
        let data = vec![0xBB; 480];
        let leaf = LeafNode::account_state(key, data.clone());
        let record = super::serialize_leaf(&key, &data);

        let node = super::deserialize_node(&record, &leaf.hash(), LeafNode::account_state).unwrap();
        match &node {
            SHAMapNode::Leaf(l) => {
                assert_eq!(*l.key(), key);
                assert_eq!(l.data(), &data[..]);
            }
            _ => panic!("480-byte leaf must not decode as an inner node"),
        }
    }

    #[test]
    fn deserialize_too_short_errors() {
        let bytes = vec![1, 2, 3]; // less than 32 bytes
        let hash = Hash256::new([0xFF; 32]);
        let result = super::deserialize_node(&bytes, &hash, LeafNode::account_state);
        assert!(result.is_err());
    }
}
