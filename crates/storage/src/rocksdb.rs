use std::path::Path;
use std::sync::Arc;

use rocksdb::{ColumnFamilyDescriptor, DB, Options};

use crate::batch::{BatchEntry, WriteBatch};
use crate::error::StorageError;
use crate::kvstore::KvStore;

/// Column family names for XRPL node storage.
pub const CF_DEFAULT: &str = "default";
pub const CF_HEADERS: &str = "headers";
pub const CF_META: &str = "meta";

const COLUMN_FAMILIES: &[&str] = &[CF_DEFAULT, CF_HEADERS, CF_META];

/// RocksDB-backed key-value store.
///
/// Uses column families to separate SHAMap nodes, ledger headers, and metadata.
pub struct RocksDbStore {
    db: Arc<DB>,
}

impl RocksDbStore {
    /// Open or create a RocksDB database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let cf_descriptors: Vec<ColumnFamilyDescriptor> = COLUMN_FAMILIES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, cf_descriptors)?;
        Ok(Self { db: Arc::new(db) })
    }

    /// Get a value from a specific column family.
    pub fn get_cf(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| StorageError::Backend(format!("column family '{cf}' not found")))?;
        Ok(self.db.get_cf(&handle, key)?)
    }

    /// Put a value into a specific column family.
    pub fn put_cf(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        let handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| StorageError::Backend(format!("column family '{cf}' not found")))?;
        self.db.put_cf(&handle, key, value)?;
        Ok(())
    }

    /// Delete a key from a specific column family.
    pub fn delete_cf(&self, cf: &str, key: &[u8]) -> Result<(), StorageError> {
        let handle = self
            .db
            .cf_handle(cf)
            .ok_or_else(|| StorageError::Backend(format!("column family '{cf}' not found")))?;
        self.db.delete_cf(&handle, key)?;
        Ok(())
    }
}

/// Default `KvStore` impl operates on the default column family.
impl KvStore for RocksDbStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(self.db.get(key)?)
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        self.db.put(key, value)?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        self.db.delete(key)?;
        Ok(())
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StorageError> {
        let mut wb = rocksdb::WriteBatch::default();
        for entry in batch.iter() {
            match entry {
                BatchEntry::Put { key, value } => wb.put(key, value),
                BatchEntry::Delete { key } => wb.delete(key),
            }
        }
        self.db.write(wb)?;
        Ok(())
    }

    fn exists(&self, key: &[u8]) -> Result<bool, StorageError> {
        Ok(self.db.get_pinned(key)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, RocksDbStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = RocksDbStore::open(dir.path()).unwrap();
        (dir, store)
    }

    #[test]
    fn basic_operations() {
        let (_dir, store) = temp_db();

        assert_eq!(store.get(b"key").unwrap(), None);
        store.put(b"key", b"value").unwrap();
        assert_eq!(store.get(b"key").unwrap(), Some(b"value".to_vec()));

        store.delete(b"key").unwrap();
        assert_eq!(store.get(b"key").unwrap(), None);
    }

    #[test]
    fn column_family_operations() {
        let (_dir, store) = temp_db();

        store.put_cf(CF_HEADERS, b"seq1", b"header_data").unwrap();
        assert_eq!(
            store.get_cf(CF_HEADERS, b"seq1").unwrap(),
            Some(b"header_data".to_vec())
        );

        // Default CF is separate
        assert_eq!(store.get(b"seq1").unwrap(), None);
    }

    #[test]
    fn write_batch_operations() {
        let (_dir, store) = temp_db();

        let mut batch = WriteBatch::new();
        batch.put(b"a".to_vec(), b"1".to_vec());
        batch.put(b"b".to_vec(), b"2".to_vec());
        store.write_batch(&batch).unwrap();

        assert_eq!(store.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(store.get(b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();

        {
            let store = RocksDbStore::open(dir.path()).unwrap();
            store.put(b"persist", b"data").unwrap();
        }

        let store = RocksDbStore::open(dir.path()).unwrap();
        assert_eq!(store.get(b"persist").unwrap(), Some(b"data".to_vec()));
    }
}
