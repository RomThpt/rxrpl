/// XRPL-aware node storage with LRU caching.
///
/// Provides a `NodeDatabase` trait for storing SHAMap nodes, with:
/// - `MemoryNodeDatabase`: In-memory backend for testing
/// - `PersistentNodeDatabase`: Wraps any `KvStore` backend
/// - `CachedNodeStore`: LRU caching layer implementing `shamap::NodeStore`
pub mod backend;
pub mod batch;
pub mod cache;
pub mod database;
pub mod error;

pub use backend::{MemoryNodeDatabase, PersistentNodeDatabase};
pub use batch::NodeBatch;
pub use cache::{CacheConfig, CachedNodeStore};
pub use database::NodeDatabase;
pub use error::NodeStoreError;
