use sqlx::PgPool;
use sqlx::Row;
use tokio::runtime::Handle;

use crate::error::StorageError;
use crate::txstore::{TransactionRecord, TxStore};

/// PostgreSQL-backed transaction history store.
///
/// Uses `block_on` to bridge the sync `TxStore` trait to async sqlx queries,
/// since the calling code (node ledger close loop) is sync.
pub struct PostgresStore {
    pool: PgPool,
    rt: Handle,
}

impl PostgresStore {
    /// Connect to a PostgreSQL database and initialize the schema.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        let pool = PgPool::connect(url).await?;
        let store = Self {
            pool,
            rt: Handle::current(),
        };
        store.init_schema().await?;
        Ok(store)
    }

    async fn init_schema(&self) -> Result<(), StorageError> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS transactions (
                tx_hash     BYTEA PRIMARY KEY,
                ledger_seq  INTEGER NOT NULL,
                tx_index    INTEGER NOT NULL,
                tx_blob     BYTEA NOT NULL,
                meta_blob   BYTEA NOT NULL
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS account_transactions (
                account     BYTEA NOT NULL,
                ledger_seq  INTEGER NOT NULL,
                tx_index    INTEGER NOT NULL,
                tx_hash     BYTEA NOT NULL,
                PRIMARY KEY (account, ledger_seq, tx_index)
            )",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_tx_ledger
                ON transactions (ledger_seq, tx_index)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_acct_tx_hash
                ON account_transactions (tx_hash)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

impl TxStore for PostgresStore {
    fn insert_transaction(
        &self,
        tx_hash: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_blob: &[u8],
        meta_blob: &[u8],
    ) -> Result<(), StorageError> {
        let tx_hash = tx_hash.to_vec();
        let tx_blob = tx_blob.to_vec();
        let meta_blob = meta_blob.to_vec();
        self.rt.block_on(async {
            sqlx::query(
                "INSERT INTO transactions (tx_hash, ledger_seq, tx_index, tx_blob, meta_blob)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (tx_hash) DO UPDATE SET
                    ledger_seq = EXCLUDED.ledger_seq,
                    tx_index = EXCLUDED.tx_index,
                    tx_blob = EXCLUDED.tx_blob,
                    meta_blob = EXCLUDED.meta_blob",
            )
            .bind(&tx_hash)
            .bind(ledger_seq as i32)
            .bind(tx_index as i32)
            .bind(&tx_blob)
            .bind(&meta_blob)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn insert_account_transaction(
        &self,
        account: &[u8],
        ledger_seq: u32,
        tx_index: u32,
        tx_hash: &[u8],
    ) -> Result<(), StorageError> {
        let account = account.to_vec();
        let tx_hash = tx_hash.to_vec();
        self.rt.block_on(async {
            sqlx::query(
                "INSERT INTO account_transactions (account, ledger_seq, tx_index, tx_hash)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (account, ledger_seq, tx_index) DO UPDATE SET
                    tx_hash = EXCLUDED.tx_hash",
            )
            .bind(&account)
            .bind(ledger_seq as i32)
            .bind(tx_index as i32)
            .bind(&tx_hash)
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    fn get_transaction(
        &self,
        tx_hash: &[u8],
    ) -> Result<Option<TransactionRecord>, StorageError> {
        let tx_hash = tx_hash.to_vec();
        self.rt.block_on(async {
            let row = sqlx::query(
                "SELECT tx_hash, ledger_seq, tx_index, tx_blob, meta_blob
                 FROM transactions WHERE tx_hash = $1",
            )
            .bind(&tx_hash)
            .fetch_optional(&self.pool)
            .await?;

            Ok(row.map(|r| TransactionRecord {
                tx_hash: r.get("tx_hash"),
                ledger_seq: r.get::<i32, _>("ledger_seq") as u32,
                tx_index: r.get::<i32, _>("tx_index") as u32,
                tx_blob: r.get("tx_blob"),
                meta_blob: r.get("meta_blob"),
            }))
        })
    }

    fn get_account_transactions_with_marker(
        &self,
        account: &[u8],
        limit: u32,
        marker_ledger_seq: Option<u32>,
        marker_tx_index: Option<u32>,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let account = account.to_vec();
        self.rt.block_on(async {
            let rows = if let (Some(m_seq), Some(m_idx)) = (marker_ledger_seq, marker_tx_index) {
                sqlx::query(
                    "SELECT tx_hash FROM account_transactions
                     WHERE account = $1
                       AND (ledger_seq < $2 OR (ledger_seq = $2 AND tx_index < $3))
                     ORDER BY ledger_seq DESC, tx_index DESC
                     LIMIT $4",
                )
                .bind(&account)
                .bind(m_seq as i32)
                .bind(m_idx as i32)
                .bind(limit as i32)
                .fetch_all(&self.pool)
                .await?
            } else {
                sqlx::query(
                    "SELECT tx_hash FROM account_transactions
                     WHERE account = $1
                     ORDER BY ledger_seq DESC, tx_index DESC
                     LIMIT $2",
                )
                .bind(&account)
                .bind(limit as i32)
                .fetch_all(&self.pool)
                .await?
            };

            Ok(rows.iter().map(|r| r.get("tx_hash")).collect())
        })
    }

    fn get_account_transactions(
        &self,
        account: &[u8],
        limit: u32,
    ) -> Result<Vec<Vec<u8>>, StorageError> {
        let account = account.to_vec();
        self.rt.block_on(async {
            let rows = sqlx::query(
                "SELECT tx_hash FROM account_transactions
                 WHERE account = $1
                 ORDER BY ledger_seq DESC, tx_index DESC
                 LIMIT $2",
            )
            .bind(&account)
            .bind(limit as i32)
            .fetch_all(&self.pool)
            .await?;

            Ok(rows.iter().map(|r| r.get("tx_hash")).collect())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<PostgresStore>();
    }
}
