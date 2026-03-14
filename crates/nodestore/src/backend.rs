use std::collections::HashMap;
use std::sync::RwLock;

use rxrpl_primitives::Hash256;
use rxrpl_storage::KvStore;

use crate::batch::NodeBatch;
use crate::database::NodeDatabase;
use crate::error::NodeStoreError;

/// In-memory node database for testing.
pub struct MemoryNodeDatabase {
    data: RwLock<HashMap<Hash256, Vec<u8>>>,
}

impl MemoryNodeDatabase {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryNodeDatabase {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeDatabase for MemoryNodeDatabase {
    fn fetch_node(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, NodeStoreError> {
        let data = self
            .data
            .read()
            .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
        Ok(data.get(hash).cloned())
    }

    fn store_batch(&self, batch: &NodeBatch) -> Result<(), NodeStoreError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
        for (hash, bytes) in batch.iter() {
            data.insert(*hash, bytes.to_vec());
        }
        Ok(())
    }

    fn exists(&self, hash: &Hash256) -> Result<bool, NodeStoreError> {
        let data = self
            .data
            .read()
            .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
        Ok(data.contains_key(hash))
    }
}

/// Persistent node database backed by a `KvStore`.
///
/// Stores nodes keyed by their Hash256 bytes in the underlying KV store.
pub struct PersistentNodeDatabase<S: KvStore> {
    store: S,
}

impl<S: KvStore> PersistentNodeDatabase<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KvStore> NodeDatabase for PersistentNodeDatabase<S> {
    fn fetch_node(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, NodeStoreError> {
        Ok(self.store.get(hash.as_bytes())?)
    }

    fn store_batch(&self, batch: &NodeBatch) -> Result<(), NodeStoreError> {
        let mut wb = rxrpl_storage::WriteBatch::with_capacity(batch.len());
        for (hash, data) in batch.iter() {
            wb.put(hash.as_bytes().to_vec(), data.to_vec());
        }
        self.store.write_batch(&wb)?;
        Ok(())
    }

    fn exists(&self, hash: &Hash256) -> Result<bool, NodeStoreError> {
        Ok(self.store.exists(hash.as_bytes())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_backend_round_trip() {
        let db = MemoryNodeDatabase::new();
        let hash = Hash256::new([0xAA; 32]);
        let data = vec![1, 2, 3, 4];

        let mut batch = NodeBatch::new();
        batch.add(hash, data.clone());
        db.store_batch(&batch).unwrap();

        assert_eq!(db.fetch_node(&hash).unwrap(), Some(data));
        assert!(db.exists(&hash).unwrap());
    }

    #[test]
    fn memory_backend_missing() {
        let db = MemoryNodeDatabase::new();
        let hash = Hash256::new([0xBB; 32]);
        assert_eq!(db.fetch_node(&hash).unwrap(), None);
        assert!(!db.exists(&hash).unwrap());
    }

    #[test]
    fn persistent_backend_round_trip() {
        let store = rxrpl_storage::MemoryStore::new();
        let db = PersistentNodeDatabase::new(store);
        let hash = Hash256::new([0xCC; 32]);
        let data = vec![5, 6, 7, 8];

        let mut batch = NodeBatch::new();
        batch.add(hash, data.clone());
        db.store_batch(&batch).unwrap();

        assert_eq!(db.fetch_node(&hash).unwrap(), Some(data));
        assert!(db.exists(&hash).unwrap());
    }
}
