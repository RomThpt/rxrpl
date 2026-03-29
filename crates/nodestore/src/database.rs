use rxrpl_primitives::Hash256;

use crate::batch::NodeBatch;
use crate::error::NodeStoreError;

/// Trait for XRPL-aware node storage.
///
/// Unlike the low-level `KvStore`, this works with `Hash256` keys
/// and understands SHAMap node semantics.
pub trait NodeDatabase: Send + Sync + 'static {
    /// Fetch a node by its hash.
    fn fetch_node(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, NodeStoreError>;

    /// Store a batch of nodes atomically.
    fn store_batch(&self, batch: &NodeBatch) -> Result<(), NodeStoreError>;

    /// Check if a node exists by hash.
    fn exists(&self, hash: &Hash256) -> Result<bool, NodeStoreError> {
        Ok(self.fetch_node(hash)?.is_some())
    }

    /// Delete a batch of nodes by hash. Used by ledger history pruning.
    fn delete_batch(&self, hashes: &[Hash256]) -> Result<(), NodeStoreError>;
}
