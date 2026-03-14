use crate::batch::WriteBatch;
use crate::error::StorageError;

/// Pluggable key-value store backend.
///
/// Implementations must be thread-safe. All operations use byte slices
/// for maximum flexibility across backends.
pub trait KvStore: Send + Sync + 'static {
    /// Get the value for a key, or `None` if not present.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;

    /// Store a key-value pair, overwriting any existing value.
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;

    /// Delete a key. No error if the key does not exist.
    fn delete(&self, key: &[u8]) -> Result<(), StorageError>;

    /// Apply a batch of writes atomically.
    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StorageError>;

    /// Check if a key exists without reading the value.
    fn exists(&self, key: &[u8]) -> Result<bool, StorageError> {
        Ok(self.get(key)?.is_some())
    }
}
