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

    /// Get the ledger sequence number.
    fn seq(&self) -> u32;

    /// Get the fee settings for this ledger.
    fn fees(&self) -> &FeeSettings;

    /// Get the total drops (XRP supply) in this ledger.
    fn drops(&self) -> u64;

    /// Get the parent ledger close time.
    fn parent_close_time(&self) -> u32;
}
