use std::collections::HashMap;
use std::sync::RwLock;

use crate::batch::{BatchEntry, WriteBatch};
use crate::error::StorageError;
use crate::kvstore::KvStore;

/// In-memory key-value store backed by a `HashMap`.
///
/// Useful for testing and ephemeral state. Not persisted across restarts.
pub struct MemoryStore {
    data: RwLock<HashMap<Vec<u8>, Vec<u8>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KvStore for MemoryStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let data = self
            .data
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(data.get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        data.insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        data.remove(key);
        Ok(())
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StorageError> {
        let mut data = self
            .data
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        for entry in batch.iter() {
            match entry {
                BatchEntry::Put { key, value } => {
                    data.insert(key.to_vec(), value.to_vec());
                }
                BatchEntry::Delete { key } => {
                    data.remove(key);
                }
            }
        }
        Ok(())
    }

    fn exists(&self, key: &[u8]) -> Result<bool, StorageError> {
        let data = self
            .data
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(data.contains_key(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_missing() {
        let store = MemoryStore::new();
        assert_eq!(store.get(b"missing").unwrap(), None);
    }

    #[test]
    fn put_and_get() {
        let store = MemoryStore::new();
        store.put(b"key", b"value").unwrap();
        assert_eq!(store.get(b"key").unwrap(), Some(b"value".to_vec()));
    }

    #[test]
    fn put_overwrites() {
        let store = MemoryStore::new();
        store.put(b"key", b"v1").unwrap();
        store.put(b"key", b"v2").unwrap();
        assert_eq!(store.get(b"key").unwrap(), Some(b"v2".to_vec()));
    }

    #[test]
    fn delete_existing() {
        let store = MemoryStore::new();
        store.put(b"key", b"value").unwrap();
        store.delete(b"key").unwrap();
        assert_eq!(store.get(b"key").unwrap(), None);
    }

    #[test]
    fn delete_missing_is_ok() {
        let store = MemoryStore::new();
        store.delete(b"missing").unwrap();
    }

    #[test]
    fn exists_check() {
        let store = MemoryStore::new();
        assert!(!store.exists(b"key").unwrap());
        store.put(b"key", b"value").unwrap();
        assert!(store.exists(b"key").unwrap());
    }

    #[test]
    fn write_batch_atomic() {
        let store = MemoryStore::new();
        store.put(b"old", b"data").unwrap();

        let mut batch = WriteBatch::new();
        batch.put(b"k1".to_vec(), b"v1".to_vec());
        batch.put(b"k2".to_vec(), b"v2".to_vec());
        batch.delete(b"old".to_vec());
        store.write_batch(&batch).unwrap();

        assert_eq!(store.get(b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(store.get(b"k2").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(store.get(b"old").unwrap(), None);
    }
}
