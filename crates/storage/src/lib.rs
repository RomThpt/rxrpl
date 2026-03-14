/// Key-value store abstraction with pluggable backends.
///
/// Provides a `KvStore` trait for generic key-value storage, with:
/// - `MemoryStore`: In-memory HashMap backend (always available)
/// - `RocksDbStore`: Persistent RocksDB backend (feature = "rocksdb")
/// - `SqliteStore`: Relational transaction history (feature = "sqlite")
pub mod batch;
pub mod error;
pub mod kvstore;
pub mod memory;

#[cfg(feature = "rocksdb")]
pub mod rocksdb;

#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use batch::{BatchEntry, WriteBatch};
pub use error::StorageError;
pub use kvstore::KvStore;
pub use memory::MemoryStore;

#[cfg(feature = "rocksdb")]
pub use rocksdb::RocksDbStore;

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteStore;
