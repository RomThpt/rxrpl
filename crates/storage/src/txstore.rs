use crate::error::StorageError;

/// A transaction record from the database.
#[derive(Debug)]
pub struct TransactionRecord {
    pub tx_hash: Vec<u8>,
    pub ledger_seq: u32,
    pub tx_index: u32,
    pub tx_blob: Vec<u8>,
    pub meta_blob: Vec<u8>,
}

/// Trait for transaction history storage backends.
///
/// Provides methods for indexing and querying transactions by hash or account.
/// Implementations must be thread-safe (Send + Sync).
pub trait TxStore: Send + Sync + 'static {
    /// Insert a transaction record.
    fn insert_transaction(
        &self,
        tx_hash: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError>;

    /// Insert an account-transaction mapping.
    fn insert_account_transaction(
        &self,
        account: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_hash: &[u8],
    ) -> Result<(), StorageError>;

    /// Look up a transaction by hash.
    fn get_transaction(&self, tx_hash: &[u8]) -> Result<Option<TransactionRecord>, StorageError>;

    /// Get transaction hashes for an account with marker-based pagination.
    ///
    /// When `marker_ledger_seq` and `marker_tx_index` are provided, results start
    /// after that position. Returns tx hashes in reverse chronological order.
    fn get_account_transactions_with_marker(
        &self,
        account: &[u8],
        limit: u32,
        marker_ledger_seq: Option<u32>,
        marker_tx_index: Option<u32>,
    ) -> Result<Vec<Vec<u8>>, StorageError>;

    /// Get transaction hashes for an account in reverse chronological order.
    fn get_account_transactions(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<Vec<u8>>, StorageError>;
}
