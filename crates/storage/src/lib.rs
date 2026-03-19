/// Key-value store abstraction with pluggable backends.
///
/// Provides a `KvStore` trait for generic key-value storage, with:
/// - `MemoryStore`: In-memory HashMap backend (always available)
/// - `RocksDbStore`: Persistent RocksDB backend (feature = "rocksdb")
/// - `SqliteStore`: Relational transaction history (feature = "sqlite")
/// - `PostgresStore`: PostgreSQL transaction history (feature = "postgres")
pub mod batch;
pub mod error;
pub mod kvstore;
pub mod memory;
pub mod txstore;

#[cfg(feature = "rocksdb")]
pub mod rocksdb;

#[cfg(feature = "sqlite")]
pub mod sqlite;

#[cfg(feature = "postgres")]
pub mod postgres;

pub use batch::{BatchEntry, WriteBatch};
pub use error::StorageError;
pub use kvstore::KvStore;
pub use memory::MemoryStore;
pub use txstore::{TransactionRecord, TxStore};

#[cfg(feature = "rocksdb")]
pub use rocksdb::RocksDbStore;

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;

#[cfg(feature = "postgres")]
pub use postgres::PostgresStore;
