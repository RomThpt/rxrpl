use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use rxrpl_primitives::Hash256;

use crate::batch::NodeBatch;
use crate::database::NodeDatabase;
use crate::error::NodeStoreError;

const DEFAULT_POSITIVE_CACHE_SIZE: usize = 65536;
const DEFAULT_NEGATIVE_CACHE_SIZE: usize = 8192;

/// Configuration for cache sizes.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub positive_cache_size: usize,
    pub negative_cache_size: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            positive_cache_size: DEFAULT_POSITIVE_CACHE_SIZE,
            negative_cache_size: DEFAULT_NEGATIVE_CACHE_SIZE,
        }
    }
}

/// A caching wrapper around a `NodeDatabase`.
///
/// Maintains an LRU positive cache (hash -> data) and a negative cache
/// (hash -> known-missing) to reduce backend lookups.
pub struct CachedNodeStore<D: NodeDatabase> {
    db: D,
    positive: Mutex<LruCache<Hash256, Vec<u8>>>,
    negative: Mutex<LruCache<Hash256, ()>>,
}

impl<D: NodeDatabase> CachedNodeStore<D> {
    pub fn new(db: D, config: CacheConfig) -> Self {
        let pos_cap = NonZeroUsize::new(config.positive_cache_size.max(1)).unwrap();
        let neg_cap = NonZeroUsize::new(config.negative_cache_size.max(1)).unwrap();
        Self {
            db,
            positive: Mutex::new(LruCache::new(pos_cap)),
            negative: Mutex::new(LruCache::new(neg_cap)),
        }
    }

    pub fn with_defaults(db: D) -> Self {
        Self::new(db, CacheConfig::default())
    }

    /// Fetch a node, checking caches first.
    pub fn fetch(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, NodeStoreError> {
        // Check positive cache
        {
            let mut cache = self
                .positive
                .lock()
                .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
            if let Some(data) = cache.get(hash) {
                return Ok(Some(data.clone()));
            }
        }

        // Check negative cache
        {
            let mut cache = self
                .negative
                .lock()
                .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
            if cache.get(hash).is_some() {
                return Ok(None);
            }
        }

        // Backend lookup
        match self.db.fetch_node(hash)? {
            Some(data) => {
                let mut cache = self
                    .positive
                    .lock()
                    .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
                cache.put(*hash, data.clone());
                Ok(Some(data))
            }
            None => {
                let mut cache = self
                    .negative
                    .lock()
                    .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
                cache.put(*hash, ());
                Ok(None)
            }
        }
    }

    /// Store a batch, populating the positive cache and clearing negative cache entries.
    pub fn store(&self, batch: &NodeBatch) -> Result<(), NodeStoreError> {
        self.db.store_batch(batch)?;

        let mut pos = self
            .positive
            .lock()
            .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
        let mut neg = self
            .negative
            .lock()
            .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;

        for (hash, data) in batch.iter() {
            pos.put(*hash, data.to_vec());
            neg.pop(hash);
        }

        Ok(())
    }

    /// Clear all negative cache entries (e.g., after a ledger advance).
    pub fn clear_negative_cache(&self) {
        if let Ok(mut cache) = self.negative.lock() {
            cache.clear();
        }
    }

    /// Check existence using caches.
    pub fn exists(&self, hash: &Hash256) -> Result<bool, NodeStoreError> {
        {
            let mut cache = self
                .positive
                .lock()
                .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
            if cache.get(hash).is_some() {
                return Ok(true);
            }
        }
        {
            let mut cache = self
                .negative
                .lock()
                .map_err(|e| NodeStoreError::Encoding(e.to_string()))?;
            if cache.get(hash).is_some() {
                return Ok(false);
            }
        }
        self.db.exists(hash)
    }
}

/// Implement `shamap::NodeStore` for `CachedNodeStore` so it can be used
/// directly with SHAMap for persistent backing.
impl<D: NodeDatabase> rxrpl_shamap::NodeStore for CachedNodeStore<D> {
    fn fetch(&self, hash: &Hash256) -> Result<Option<Vec<u8>>, rxrpl_shamap::SHAMapError> {
        self.fetch(hash)
            .map_err(|_| rxrpl_shamap::SHAMapError::InvalidNode)
    }

    fn store_batch(&self, entries: &[(&Hash256, &[u8])]) -> Result<(), rxrpl_shamap::SHAMapError> {
        let mut batch = NodeBatch::with_capacity(entries.len());
        for (hash, data) in entries {
            batch.add(**hash, data.to_vec());
        }
        self.store(&batch)
            .map_err(|_| rxrpl_shamap::SHAMapError::InvalidNode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryNodeDatabase;

    fn make_store() -> CachedNodeStore<MemoryNodeDatabase> {
        let config = CacheConfig {
            positive_cache_size: 16,
            negative_cache_size: 8,
        };
        CachedNodeStore::new(MemoryNodeDatabase::new(), config)
    }

    #[test]
    fn cache_miss_then_hit() {
        let store = make_store();
        let hash = Hash256::new([0xAA; 32]);
        let data = vec![1, 2, 3];

        // Miss
        assert_eq!(store.fetch(&hash).unwrap(), None);

        // Store
        let mut batch = NodeBatch::new();
        batch.add(hash, data.clone());
        store.store(&batch).unwrap();

        // Hit from cache
        assert_eq!(store.fetch(&hash).unwrap(), Some(data));
    }

    #[test]
    fn negative_cache() {
        let store = make_store();
        let hash = Hash256::new([0xBB; 32]);

        // First miss populates negative cache
        assert_eq!(store.fetch(&hash).unwrap(), None);
        // Second miss hits negative cache (no backend call)
        assert_eq!(store.fetch(&hash).unwrap(), None);
    }

    #[test]
    fn store_clears_negative_cache() {
        let store = make_store();
        let hash = Hash256::new([0xCC; 32]);

        // Populate negative cache
        assert_eq!(store.fetch(&hash).unwrap(), None);

        // Store should clear negative entry
        let mut batch = NodeBatch::new();
        batch.add(hash, vec![42]);
        store.store(&batch).unwrap();

        assert_eq!(store.fetch(&hash).unwrap(), Some(vec![42]));
    }

    #[test]
    fn exists_uses_cache() {
        let store = make_store();
        let hash = Hash256::new([0xDD; 32]);

        assert!(!store.exists(&hash).unwrap());

        let mut batch = NodeBatch::new();
        batch.add(hash, vec![1]);
        store.store(&batch).unwrap();

        assert!(store.exists(&hash).unwrap());
    }

    #[test]
    fn clear_negative_cache_works() {
        let store = make_store();
        let hash = Hash256::new([0xEE; 32]);

        // Populate negative cache
        assert_eq!(store.fetch(&hash).unwrap(), None);

        store.clear_negative_cache();

        // After clearing, should check backend again
        // (still None since we haven't stored anything)
        assert_eq!(store.fetch(&hash).unwrap(), None);
    }

    #[test]
    fn shamap_node_store_impl() {
        use rxrpl_shamap::NodeStore;

        let store = make_store();
        let hash = Hash256::new([0xFF; 32]);
        let data = vec![10, 20, 30];

        NodeStore::store_batch(&store, &[(&hash, &data)]).unwrap();
        assert_eq!(NodeStore::fetch(&store, &hash).unwrap(), Some(data));
    }
}
