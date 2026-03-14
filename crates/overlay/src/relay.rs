use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use rxrpl_primitives::Hash256;

/// Message deduplication for relay.
///
/// Uses an LRU cache of message hashes to prevent re-broadcasting
/// messages we've already seen.
pub struct RelayFilter {
    seen: Mutex<LruCache<Hash256, ()>>,
}

impl RelayFilter {
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap();
        Self {
            seen: Mutex::new(LruCache::new(cap)),
        }
    }

    /// Check if a message should be relayed (returns true if not seen before).
    pub fn should_relay(&self, hash: &Hash256) -> bool {
        let mut seen = self.seen.lock().unwrap();
        if seen.contains(hash) {
            false
        } else {
            seen.put(*hash, ());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_messages() {
        let filter = RelayFilter::new(100);
        let hash = Hash256::new([0xAA; 32]);

        assert!(filter.should_relay(&hash)); // First time
        assert!(!filter.should_relay(&hash)); // Duplicate
    }

    #[test]
    fn different_hashes() {
        let filter = RelayFilter::new(100);
        let h1 = Hash256::new([0x01; 32]);
        let h2 = Hash256::new([0x02; 32]);

        assert!(filter.should_relay(&h1));
        assert!(filter.should_relay(&h2));
    }
}
