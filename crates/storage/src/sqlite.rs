use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

use crate::error::StorageError;
use crate::txstore::{TransactionRecord, TxStore};

/// SQLite-backed relational store for transaction history indexing.
///
/// Unlike the KvStore trait (which is pure key-value), this provides
/// relational queries for transaction lookups by account, ledger, etc.
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
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
            CREATE TABLE IF NOT EXISTS transactions (
                tx_hash     BLOB PRIMARY KEY,
                ledger_seq  INTEGER NOT NULL,
                tx_index    INTEGER NOT NULL,
                tx_blob     BLOB NOT NULL,
                meta_blob   BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS account_transactions (
                account     BLOB NOT NULL,
                ledger_seq  INTEGER NOT NULL,
                tx_index    INTEGER NOT NULL,
                tx_hash     BLOB NOT NULL,
                PRIMARY KEY (account, ledger_seq, tx_index)
            );

            CREATE INDEX IF NOT EXISTS idx_tx_ledger
                ON transactions (ledger_seq, tx_index);

            CREATE INDEX IF NOT EXISTS idx_acct_tx_hash
                ON account_transactions (tx_hash);
            ",
        )?;
        Ok(())
    }
}

impl TxStore for SqliteStore {
    fn insert_transaction(
        &self,
        tx_hash: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO transactions (tx_hash, ledger_seq, tx_index, tx_blob, meta_blob)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![tx_hash, ledger_seq, tx_index, tx_blob, meta_blob],
        )?;
        Ok(())
    }

    fn insert_account_transaction(
        &self,
        account: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_hash: &[u8],
    ) -> Result<(), StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO account_transactions (account, ledger_seq, tx_index, tx_hash)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![account, ledger_seq, tx_index, tx_hash],
        )?;
        Ok(())
    }

    fn get_transaction(&self, tx_hash: &[u8]) -> Result<Option<TransactionRecord>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT tx_hash, ledger_seq, tx_index, tx_blob, meta_blob
             FROM transactions WHERE tx_hash = ?1",
        )?;
        let result = stmt
            .query_row(rusqlite::params![tx_hash], |row| {
                Ok(TransactionRecord {
                    tx_hash: row.get(0)?,
                    ledger_seq: row.get(1)?,
                    tx_index: row.get(2)?,
                    tx_blob: row.get(3)?,
                    meta_blob: row.get(4)?,
                })
            })
            .optional()?;
        Ok(result)
    }

    fn get_account_transactions_with_marker(
        &self,
        account: &[u8],
        limit: u32,
        marker_ledger_seq: Option<u32>,
        marker_tx_index: Option<u32>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;

        let (query, params): (&str, Vec<Box<dyn rusqlite::types::ToSql>>) =
            if let (Some(m_seq), Some(m_idx)) = (marker_ledger_seq, marker_tx_index) {
                (
                    "SELECT tx_hash FROM account_transactions
                     WHERE account = ?1
                       AND (ledger_seq < ?2 OR (ledger_seq = ?2 AND tx_index < ?3))
                     ORDER BY ledger_seq DESC, tx_index DESC
                     LIMIT ?4",
                    vec![
                        Box::new(account.to_vec()),
                        Box::new(m_seq),
                        Box::new(m_idx),
                        Box::new(limit),
                    ],
                )
            } else {
                (
                    "SELECT tx_hash FROM account_transactions
                     WHERE account = ?1
                     ORDER BY ledger_seq DESC, tx_index DESC
                     LIMIT ?2",
                    vec![Box::new(account.to_vec()), Box::new(limit)],
                )
            };

        let mut stmt = conn.prepare(query)?;
        let hashes = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| row.get(0))?
            .collect::<Result<Vec<Vec<u8>>, _>>()?;
        Ok(hashes)
    }

    fn get_account_transactions(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Backend(e.to_string()))?;
        let mut stmt = conn.prepare(
            "SELECT tx_hash FROM account_transactions
             WHERE account = ?1
             ORDER BY ledger_seq DESC, tx_index DESC
             LIMIT ?2",
        )?;
        let hashes = stmt
            .query_map(rusqlite::params![account, limit], |row| row.get(0))?
            .collect::<Result<Vec<Vec<u8>>, _>>()?;
        Ok(hashes)
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
        SqliteStore::in_memory().unwrap();
    }

    #[test]
    fn insert_and_get_transaction() {
        let store = SqliteStore::in_memory().unwrap();
        let tx_hash = b"0123456789abcdef0123456789abcdef";
        let tx_blob = b"transaction_data";
        let meta_blob = b"metadata";

        store
            .insert_transaction(tx_hash, 100, 0, tx_blob, meta_blob)
            .unwrap();

        let record = store.get_transaction(tx_hash).unwrap().unwrap();
        assert_eq!(record.ledger_seq, 100);
        assert_eq!(record.tx_index, 0);
        assert_eq!(record.tx_blob, tx_blob);
        assert_eq!(record.meta_blob, meta_blob);
    }

    #[test]
    fn get_missing_transaction() {
        let store = SqliteStore::in_memory().unwrap();
        assert!(store.get_transaction(b"missing").unwrap().is_none());
    }

    #[test]
    fn account_transaction_lookup() {
        let store = SqliteStore::in_memory().unwrap();
        let account = b"account_id_bytes_here";
        let tx1 = b"tx_hash_1_padded_to_32_bytes!!!_";
        let tx2 = b"tx_hash_2_padded_to_32_bytes!!!_";

        store
            .insert_account_transaction(account, 100, 0, tx1)
            .unwrap();
        store
            .insert_account_transaction(account, 101, 0, tx2)
            .unwrap();

        let results = store.get_account_transactions(account, 10).unwrap();
        assert_eq!(results.len(), 2);
        // Reverse chronological
        assert_eq!(results[0], tx2);
        assert_eq!(results[1], tx1);
    }
}
