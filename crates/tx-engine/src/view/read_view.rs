use rxrpl_primitives::Hash256;

use crate::fees::FeeSettings;

/// Read-only view into a ledger's state.
///
/// Provides access to ledger entries without allowing modifications.
/// Used by transactors during the `preclaim` phase and for read-only queries.
pub trait ReadView {
    /// Read a state entry by key.
    fn read(&self, key: &Hash256) -> Option<Vec<u8>>;

    /// Check if a state entry exists.
    fn exists(&self, key: &Hash256) -> bool {
        self.read(key).is_some()
    }

    /// The smallest state key strictly greater than `key`, or `None`. Used to
    /// walk order-book quality directories (rippled's `ReadView::succ`). The
    /// default returns `None` for views without ordered traversal.
    fn succ(&self, _key: &Hash256) -> Option<Hash256> {
        None
    }

    /// Get the ledger sequence number.
    fn seq(&self) -> u32;

    /// Get the fee settings for this ledger.
    fn fees(&self) -> &FeeSettings;

    /// Get the total drops (XRP supply) in this ledger.
    fn drops(&self) -> u64;

    /// Get the parent ledger close time.
    fn parent_close_time(&self) -> u32;

    /// Get the close time of the ledger being built (the open ledger). Defaults
    /// to the parent close time for views that don't track it.
    fn close_time(&self) -> u32 {
        self.parent_close_time()
    }

    /// Get the parent ledger hash (this ledger's `parentHash`). Used to derive
    /// pseudo-account addresses (e.g. AMM accounts). Defaults to zero for views
    /// without a header.
    fn parent_hash(&self) -> Hash256 {
        Hash256::ZERO
    }
}
