use std::collections::HashMap;
use std::sync::RwLock;

use rxrpl_primitives::Hash256;

use crate::error::SHAMapError;

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
}
