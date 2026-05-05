use std::collections::HashMap;
use std::sync::RwLock;

use crate::error::StorageError;

/// A stored ledger header record.
#[derive(Clone, Debug)]
pub struct LedgerHeaderRecord {
    pub sequence: u32,
    pub hash: Vec<u8>,
    pub header_blob: Vec<u8>,
}

/// A stored transaction record within a ledger.
#[derive(Clone, Debug)]
pub struct LedgerTxRecord {
    pub tx_hash: Vec<u8>,
    pub ledger_seq: u32,
    pub tx_blob: Vec<u8>,
    pub meta_blob: Vec<u8>,
}

/// Trait for ledger history storage backends used by reporting mode.
///
/// Provides methods for storing and retrieving validated ledger headers
/// and their associated transactions. Implementations must be thread-safe.
pub trait LedgerStore: Send + Sync + 'static {
    /// Store a validated ledger header.
    fn store_ledger(&self, seq: u32, hash: &[u8], header_blob: &[u8]) -> Result<(), StorageError>;

    /// Retrieve a ledger header by sequence number.
    fn get_ledger_header(&self, seq: u32) -> Result<Option<LedgerHeaderRecord>, StorageError>;

    /// Store a transaction associated with a ledger.
    fn store_tx(
        &self,
        hash: &[u8],
        ledger_seq: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError>;

    /// Look up a transaction by hash.
    fn get_tx(&self, hash: &[u8]) -> Result<Option<LedgerTxRecord>, StorageError>;

    /// Get transactions for an account, up to `limit` results.
    fn get_account_txs(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<LedgerTxRecord>, StorageError>;

    /// Index a transaction against an account for later retrieval.
    fn index_account_tx(&self, account: &[u8], tx_hash: &[u8]) -> Result<(), StorageError>;

    /// Return the latest stored ledger sequence, or None if empty.
    fn latest_sequence(&self) -> Result<Option<u32>, StorageError>;
}

/// In-memory implementation of `LedgerStore` for testing.
pub struct InMemoryLedgerStore {
    headers: RwLock<HashMap<u32, LedgerHeaderRecord>>,
    txs: RwLock<HashMap<Vec<u8>, LedgerTxRecord>>,
    account_txs: RwLock<HashMap<Vec<u8>, Vec<Vec<u8>>>>,
}

impl InMemoryLedgerStore {
    pub fn new() -> Self {
        Self {
            headers: RwLock::new(HashMap::new()),
            txs: RwLock::new(HashMap::new()),
            account_txs: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryLedgerStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerStore for InMemoryLedgerStore {
    fn store_ledger(&self, seq: u32, hash: &[u8], header_blob: &[u8]) -> Result<(), StorageError> {
        let record = LedgerHeaderRecord {
            sequence: seq,
            hash: hash.to_vec(),
            header_blob: header_blob.to_vec(),
        };
        self.headers
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?
            .insert(seq, record);
        Ok(())
    }

    fn get_ledger_header(&self, seq: u32) -> Result<Option<LedgerHeaderRecord>, StorageError> {
        let guard = self
            .headers
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(guard.get(&seq).cloned())
    }

    fn store_tx(
        &self,
        hash: &[u8],
        ledger_seq: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError> {
        let record = LedgerTxRecord {
            tx_hash: hash.to_vec(),
            ledger_seq,
            tx_blob: tx_blob.to_vec(),
            meta_blob: meta_blob.to_vec(),
        };
        self.txs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?
            .insert(hash.to_vec(), record);
        Ok(())
    }

    fn get_tx(&self, hash: &[u8]) -> Result<Option<LedgerTxRecord>, StorageError> {
        let guard = self
            .txs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(guard.get(hash).cloned())
    }

    fn get_account_txs(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<LedgerTxRecord>, StorageError> {
        let acct_guard = self
            .account_txs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        let tx_hashes = match acct_guard.get(account) {
            Some(hashes) => hashes.clone(),
            None => return Ok(Vec::new()),
        };
        drop(acct_guard);

        let tx_guard = self
            .txs
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        let mut results = Vec::new();
        for hash in tx_hashes.iter().rev().take(limit as usize) {
            if let Some(record) = tx_guard.get(hash) {
                results.push(record.clone());
            }
        }
        Ok(results)
    }

    fn index_account_tx(&self, account: &[u8], tx_hash: &[u8]) -> Result<(), StorageError> {
        self.account_txs
            .write()
            .map_err(|e| StorageError::Backend(e.to_string()))?
            .entry(account.to_vec())
            .or_default()
            .push(tx_hash.to_vec());
        Ok(())
    }

    fn latest_sequence(&self) -> Result<Option<u32>, StorageError> {
        let guard = self
            .headers
            .read()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        Ok(guard.keys().max().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_ledger_header() {
        let store = InMemoryLedgerStore::new();
        let hash = b"ledger_hash_32bytes_placeholder!";
        let header = b"serialized_header_data";

        store.store_ledger(42, hash, header).unwrap();

        let record = store.get_ledger_header(42).unwrap().unwrap();
        assert_eq!(record.sequence, 42);
        assert_eq!(record.hash, hash.to_vec());
        assert_eq!(record.header_blob, header.to_vec());
    }

    #[test]
    fn get_missing_ledger_returns_none() {
        let store = InMemoryLedgerStore::new();
        assert!(store.get_ledger_header(999).unwrap().is_none());
    }

    #[test]
    fn store_and_retrieve_tx() {
        let store = InMemoryLedgerStore::new();
        let tx_hash = b"tx_hash_placeholder_here!!!!!!!!";
        let tx_blob = b"tx_blob_data";
        let meta_blob = b"meta_blob_data";

        store.store_tx(tx_hash, 10, tx_blob, meta_blob).unwrap();

        let record = store.get_tx(tx_hash).unwrap().unwrap();
        assert_eq!(record.tx_hash, tx_hash.to_vec());
        assert_eq!(record.ledger_seq, 10);
        assert_eq!(record.tx_blob, tx_blob.to_vec());
        assert_eq!(record.meta_blob, meta_blob.to_vec());
    }

    #[test]
    fn get_missing_tx_returns_none() {
        let store = InMemoryLedgerStore::new();
        assert!(store.get_tx(b"nonexistent").unwrap().is_none());
    }

    #[test]
    fn account_tx_indexing_and_retrieval() {
        let store = InMemoryLedgerStore::new();
        let account = b"account_id_20_bytes!";
        let tx1 = b"tx_hash_1___________________________";
        let tx2 = b"tx_hash_2___________________________";

        store.store_tx(tx1, 5, b"blob1", b"meta1").unwrap();
        store.store_tx(tx2, 6, b"blob2", b"meta2").unwrap();
        store.index_account_tx(account, tx1).unwrap();
        store.index_account_tx(account, tx2).unwrap();

        let results = store.get_account_txs(account, 10).unwrap();
        assert_eq!(results.len(), 2);
        // Returned in reverse order (most recent first)
        assert_eq!(results[0].tx_hash, tx2.to_vec());
        assert_eq!(results[1].tx_hash, tx1.to_vec());
    }

    #[test]
    fn account_tx_respects_limit() {
        let store = InMemoryLedgerStore::new();
        let account = b"account_id_20_bytes!";

        for i in 0..5u8 {
            let hash = [i; 32];
            store.store_tx(&hash, i as u32, b"blob", b"meta").unwrap();
            store.index_account_tx(account, &hash).unwrap();
        }

        let results = store.get_account_txs(account, 2).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn latest_sequence_empty_store() {
        let store = InMemoryLedgerStore::new();
        assert_eq!(store.latest_sequence().unwrap(), None);
    }

    #[test]
    fn latest_sequence_tracks_max() {
        let store = InMemoryLedgerStore::new();
        store.store_ledger(10, b"h1", b"hdr1").unwrap();
        store.store_ledger(20, b"h2", b"hdr2").unwrap();
        store.store_ledger(15, b"h3", b"hdr3").unwrap();

        assert_eq!(store.latest_sequence().unwrap(), Some(20));
    }
}
