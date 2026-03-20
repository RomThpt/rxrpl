use std::collections::HashMap;
use std::sync::RwLock;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;
use crate::inner_node::InnerNode;
use crate::leaf_node::LeafNode;
use crate::node::SHAMapNode;
use crate::node_id::BRANCH_FACTOR;

/// Deserialize a node from its stored byte representation.
///
/// - 512 bytes -> inner node (16 x 32-byte child hashes)
/// - >= 32 bytes (not 512) -> leaf node (key || data)
/// - < 32 bytes -> error
pub fn deserialize_node(
    bytes: &[u8],
    hash: &Hash256,
    leaf_ctor: fn(Hash256, Vec<u8>) -> LeafNode,
) -> Result<SHAMapNode, SHAMapError> {
    if bytes.len() == BRANCH_FACTOR * 32 {
        // Inner node: 16 child hashes
        let mut inner = InnerNode::new();
        for i in 0..BRANCH_FACTOR {
            let start = i * 32;
            let child_hash = Hash256::new(bytes[start..start + 32].try_into().unwrap());
            if !child_hash.is_zero() {
                inner.set_child_hash(i as u8, child_hash);
            }
        }
        inner.set_cached_hash(*hash);
        Ok(SHAMapNode::inner(inner))
    } else if bytes.len() >= 32 {
        // Leaf node: key (32 bytes) || data
        let key = Hash256::new(bytes[..32].try_into().unwrap());
        let data = bytes[32..].to_vec();
        let leaf = leaf_ctor(key, data);
        Ok(SHAMapNode::Leaf(leaf))
    } else {
        Err(SHAMapError::DeserializeError)
    }
}

/// Pluggable storage backend for SHAMap nodes.
///
/// Allows future implementation of persistent backends (RocksDB, etc.).
pub trait NodeStore: Send + Sync {
    fn fetch(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, SHAMapError>;
    fn store_batch(&self, entries: &[(&Hash256, &[u8])]) -> Result<(), SHAMapError>;
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

        // Serialize: 16 x 32-byte child hashes
        let mut data = Vec::with_capacity(BRANCH_FACTOR * 32);
        for i in 0..16u8 {
            data.extend_from_slice(inner.child_hash(i).as_bytes());
        }

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

        // Serialize: key || data
        let mut bytes = Vec::new();
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&leaf_data);

        let node =
            super::deserialize_node(&bytes, &leaf_hash, LeafNode::account_state).unwrap();
        match &node {
            SHAMapNode::Leaf(deserialized) => {
                assert_eq!(*deserialized.key(), key);
                assert_eq!(deserialized.data(), &leaf_data[..]);
            }
            _ => panic!("expected leaf node"),
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
