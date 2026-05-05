//! SQLite-backed implementation of the `LedgerStore` trait.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::StorageError;
use crate::ledgerstore::{LedgerHeaderRecord, LedgerStore, LedgerTxRecord};

/// SQLite-backed persistent ledger history store for reporting mode.
pub struct SqliteLedgerStore {
    conn: Mutex<Connection>,
}

impl SqliteLedgerStore {
    /// Open or create a SQLite database at the given path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory SQLite database (for testing).
    pub fn in_memory() -> Result<Self, StorageError> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS ledger_headers (
                sequence    INTEGER PRIMARY KEY,
                hash        BLOB NOT NULL,
                header_blob BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS transactions (
                tx_hash     BLOB PRIMARY KEY,
                ledger_seq  INTEGER NOT NULL,
                tx_blob     BLOB NOT NULL,
                meta_blob   BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS account_transactions (
                account     BLOB NOT NULL,
                tx_hash     BLOB NOT NULL,
                PRIMARY KEY (account, tx_hash)
            );

            CREATE INDEX IF NOT EXISTS idx_ledger_tx_seq
                ON transactions (ledger_seq);
            ",
        )?;
        Ok(())
    }
}

impl LedgerStore for SqliteLedgerStore {
    fn store_ledger(&self, seq: u32, hash: &[u8], header_blob: &[u8]) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO ledger_headers (sequence, hash, header_blob)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![seq, hash, header_blob],
        )?;
        Ok(())
    }

    fn get_ledger_header(&self, seq: u32) -> Result<Option<LedgerHeaderRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT sequence, hash, header_blob FROM ledger_headers WHERE sequence = ?1",
        )?;
        let result = stmt
            .query_row(rusqlite::params![seq], |row| {
                Ok(LedgerHeaderRecord {
                    sequence: row.get(0)?,
                    hash: row.get(1)?,
                    header_blob: row.get(2)?,
                })
            })
            .optional()?;
        Ok(result)
    }

    fn store_tx(
        &self,
        hash: &[u8],
        ledger_seq: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO transactions (tx_hash, ledger_seq, tx_blob, meta_blob)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![hash, ledger_seq, tx_blob, meta_blob],
        )?;
        Ok(())
    }

    fn get_tx(&self, hash: &[u8]) -> Result<Option<LedgerTxRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT tx_hash, ledger_seq, tx_blob, meta_blob
             FROM transactions WHERE tx_hash = ?1",
        )?;
        let result = stmt
            .query_row(rusqlite::params![hash], |row| {
                Ok(LedgerTxRecord {
                    tx_hash: row.get(0)?,
                    ledger_seq: row.get(1)?,
                    tx_blob: row.get(2)?,
                    meta_blob: row.get(3)?,
                })
            })
            .optional()?;
        Ok(result)
    }

    fn get_account_txs(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<LedgerTxRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT t.tx_hash, t.ledger_seq, t.tx_blob, t.meta_blob
             FROM account_transactions a
             JOIN transactions t ON a.tx_hash = t.tx_hash
             WHERE a.account = ?1
             ORDER BY t.ledger_seq DESC
             LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![account, limit], |row| {
                Ok(LedgerTxRecord {
                    tx_hash: row.get(0)?,
                    ledger_seq: row.get(1)?,
                    tx_blob: row.get(2)?,
                    meta_blob: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn index_account_tx(&self, account: &[u8], tx_hash: &[u8]) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute(
            "INSERT OR IGNORE INTO account_transactions (account, tx_hash)
             VALUES (?1, ?2)",
            rusqlite::params![account, tx_hash],
        )?;
        Ok(())
    }

    fn latest_sequence(&self) -> Result<Option<u32>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare("SELECT MAX(sequence) FROM ledger_headers")?;
        let result: Option<u32> = stmt.query_row([], |row| row.get(0)).unwrap_or(None);
        Ok(result)
    }
}

/// Extension trait to convert `rusqlite::Error::QueryReturnedNoRows` to `None`.
trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creation() {
        SqliteLedgerStore::in_memory().unwrap();
    }

    #[test]
    fn store_and_retrieve_ledger_header() {
        let store = SqliteLedgerStore::in_memory().unwrap();
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
        let store = SqliteLedgerStore::in_memory().unwrap();
        assert!(store.get_ledger_header(999).unwrap().is_none());
    }

    #[test]
    fn store_and_retrieve_tx() {
        let store = SqliteLedgerStore::in_memory().unwrap();
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
        let store = SqliteLedgerStore::in_memory().unwrap();
        assert!(store.get_tx(b"nonexistent").unwrap().is_none());
    }

    #[test]
    fn account_tx_indexing_and_retrieval() {
        let store = SqliteLedgerStore::in_memory().unwrap();
        let account = b"account_id_20_bytes!";
        let tx1 = b"tx_hash_1___________________________";
        let tx2 = b"tx_hash_2___________________________";

        store.store_tx(tx1, 5, b"blob1", b"meta1").unwrap();
        store.store_tx(tx2, 6, b"blob2", b"meta2").unwrap();
        store.index_account_tx(account, tx1).unwrap();
        store.index_account_tx(account, tx2).unwrap();

        let results = store.get_account_txs(account, 10).unwrap();
        assert_eq!(results.len(), 2);
        // Returned in reverse chronological order
        assert_eq!(results[0].tx_hash, tx2.to_vec());
        assert_eq!(results[1].tx_hash, tx1.to_vec());
    }

    #[test]
    fn account_tx_respects_limit() {
        let store = SqliteLedgerStore::in_memory().unwrap();
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
        let store = SqliteLedgerStore::in_memory().unwrap();
        assert_eq!(store.latest_sequence().unwrap(), None);
    }

    #[test]
    fn latest_sequence_tracks_max() {
        let store = SqliteLedgerStore::in_memory().unwrap();
        store.store_ledger(10, b"h1", b"hdr1").unwrap();
        store.store_ledger(20, b"h2", b"hdr2").unwrap();
        store.store_ledger(15, b"h3", b"hdr3").unwrap();

        assert_eq!(store.latest_sequence().unwrap(), Some(20));
    }
}
