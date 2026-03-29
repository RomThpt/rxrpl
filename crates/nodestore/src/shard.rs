//! Ledger history sharding.
//!
//! Splits the full ledger history into fixed-size shards of `LEDGERS_PER_SHARD`
//! consecutive ledgers. Each shard is identified by an index and can be stored,
//! downloaded, and queried independently.

use std::collections::HashMap;

use rxrpl_primitives::Hash256;

/// Number of ledgers contained in a single shard.
pub const LEDGERS_PER_SHARD: u32 = 16384;

/// Return the shard index that contains ledger sequence `seq`.
///
/// Shard 0 covers sequences `0..LEDGERS_PER_SHARD`, shard 1 covers
/// `LEDGERS_PER_SHARD..2*LEDGERS_PER_SHARD`, and so on.
pub fn shard_index_for(seq: u32) -> u32 {
    seq / LEDGERS_PER_SHARD
}

/// Return the inclusive `(first_seq, last_seq)` range for a given shard index.
pub fn shard_range(index: u32) -> (u32, u32) {
    let first = index * LEDGERS_PER_SHARD;
    let last = first + LEDGERS_PER_SHARD - 1;
    (first, last)
}

/// State of a single shard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShardState {
    /// All ledgers in the shard have been stored and verified.
    Complete,
    /// Some ledgers have been stored but the shard is not yet complete.
    Incomplete {
        /// Number of ledgers stored so far.
        stored_count: u32,
    },
    /// The shard is currently being downloaded from an external source.
    Downloading,
}

/// Metadata for a single shard.
#[derive(Clone, Debug)]
pub struct ShardInfo {
    /// Shard index.
    pub index: u32,
    /// Current state of this shard.
    pub state: ShardState,
    /// First ledger sequence in this shard.
    pub first_seq: u32,
    /// Last ledger sequence in this shard.
    pub last_seq: u32,
    /// Hash of the last ledger stored (if any).
    pub last_hash: Option<Hash256>,
}

/// Simple in-memory shard data store.
///
/// Maps `(shard_index, ledger_seq)` to raw ledger data bytes.
pub struct ShardStore {
    data: HashMap<(u32, u32), Vec<u8>>,
}

impl ShardStore {
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }

    /// Insert ledger data for a specific shard and sequence.
    pub fn put(&mut self, shard_index: u32, seq: u32, data: Vec<u8>) {
        self.data.insert((shard_index, seq), data);
    }

    /// Retrieve ledger data for a specific shard and sequence.
    pub fn get(&self, shard_index: u32, seq: u32) -> Option<&[u8]> {
        self.data.get(&(shard_index, seq)).map(|v| v.as_slice())
    }

    /// Return the number of stored entries for a given shard index.
    pub fn count_for_shard(&self, shard_index: u32) -> u32 {
        self.data
            .keys()
            .filter(|(idx, _)| *idx == shard_index)
            .count() as u32
    }
}

impl Default for ShardStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Manages shard metadata and coordinates imports.
pub struct ShardManager {
    shards: HashMap<u32, ShardInfo>,
    store: ShardStore,
}

impl ShardManager {
    pub fn new() -> Self {
        Self {
            shards: HashMap::new(),
            store: ShardStore::new(),
        }
    }

    /// Return information about a specific shard, if tracked.
    pub fn shard_info(&self, index: u32) -> Option<&ShardInfo> {
        self.shards.get(&index)
    }

    /// Return all tracked shards.
    pub fn all_shards(&self) -> Vec<&ShardInfo> {
        let mut shards: Vec<_> = self.shards.values().collect();
        shards.sort_by_key(|s| s.index);
        shards
    }

    /// Return indices of all complete shards as a comma-separated string.
    pub fn complete_shards_string(&self) -> String {
        let mut indices: Vec<u32> = self
            .shards
            .values()
            .filter(|s| s.state == ShardState::Complete)
            .map(|s| s.index)
            .collect();
        indices.sort();
        if indices.is_empty() {
            "none".to_string()
        } else {
            indices
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",")
        }
    }

    /// Import a single ledger into the appropriate shard.
    ///
    /// Creates the shard entry if it does not exist, updates the stored count,
    /// and marks the shard as complete when all ledgers have been imported.
    pub fn import_ledger(&mut self, seq: u32, hash: Hash256, data: Vec<u8>) {
        let index = shard_index_for(seq);
        let (first_seq, last_seq) = shard_range(index);

        self.store.put(index, seq, data);
        let stored_count = self.store.count_for_shard(index);

        let state = if stored_count >= LEDGERS_PER_SHARD {
            ShardState::Complete
        } else {
            ShardState::Incomplete { stored_count }
        };

        let info = self.shards.entry(index).or_insert_with(|| ShardInfo {
            index,
            state: ShardState::Incomplete { stored_count: 0 },
            first_seq,
            last_seq,
            last_hash: None,
        });

        info.state = state;
        info.last_hash = Some(hash);
    }

    /// Mark a shard as downloading.
    pub fn mark_downloading(&mut self, index: u32) {
        let (first_seq, last_seq) = shard_range(index);
        let info = self.shards.entry(index).or_insert_with(|| ShardInfo {
            index,
            state: ShardState::Downloading,
            first_seq,
            last_seq,
            last_hash: None,
        });
        info.state = ShardState::Downloading;
    }

    /// Check whether a shard is complete.
    pub fn is_complete(&self, index: u32) -> bool {
        self.shards
            .get(&index)
            .map(|s| s.state == ShardState::Complete)
            .unwrap_or(false)
    }
}

impl Default for ShardManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shard_index_for() {
        assert_eq!(shard_index_for(0), 0);
        assert_eq!(shard_index_for(1), 0);
        assert_eq!(shard_index_for(LEDGERS_PER_SHARD - 1), 0);
        assert_eq!(shard_index_for(LEDGERS_PER_SHARD), 1);
        assert_eq!(shard_index_for(LEDGERS_PER_SHARD + 1), 1);
        assert_eq!(shard_index_for(2 * LEDGERS_PER_SHARD), 2);
    }

    #[test]
    fn test_shard_range() {
        assert_eq!(shard_range(0), (0, LEDGERS_PER_SHARD - 1));
        assert_eq!(
            shard_range(1),
            (LEDGERS_PER_SHARD, 2 * LEDGERS_PER_SHARD - 1)
        );
        assert_eq!(
            shard_range(3),
            (3 * LEDGERS_PER_SHARD, 4 * LEDGERS_PER_SHARD - 1)
        );
    }

    #[test]
    fn test_import_and_complete_flow() {
        let mut manager = ShardManager::new();

        // Import first ledger into shard 0
        let hash = Hash256::ZERO;
        manager.import_ledger(0, hash, vec![0u8; 32]);

        let info = manager.shard_info(0).unwrap();
        assert_eq!(info.index, 0);
        assert_eq!(info.state, ShardState::Incomplete { stored_count: 1 });
        assert!(!manager.is_complete(0));

        // Import all remaining ledgers in shard 0
        for seq in 1..LEDGERS_PER_SHARD {
            manager.import_ledger(seq, hash, vec![0u8; 32]);
        }

        assert!(manager.is_complete(0));
        let info = manager.shard_info(0).unwrap();
        assert_eq!(info.state, ShardState::Complete);
        assert_eq!(info.last_hash, Some(hash));
    }

    #[test]
    fn test_mark_downloading() {
        let mut manager = ShardManager::new();
        manager.mark_downloading(5);

        let info = manager.shard_info(5).unwrap();
        assert_eq!(info.state, ShardState::Downloading);
        assert_eq!(info.first_seq, 5 * LEDGERS_PER_SHARD);
        assert_eq!(info.last_seq, 6 * LEDGERS_PER_SHARD - 1);
    }

    #[test]
    fn test_complete_shards_string() {
        let mut manager = ShardManager::new();
        assert_eq!(manager.complete_shards_string(), "none");

        // Fill shard 0
        let hash = Hash256::ZERO;
        for seq in 0..LEDGERS_PER_SHARD {
            manager.import_ledger(seq, hash, vec![0u8; 4]);
        }

        assert_eq!(manager.complete_shards_string(), "0");

        // Partially fill shard 1
        manager.import_ledger(LEDGERS_PER_SHARD, hash, vec![0u8; 4]);
        assert_eq!(manager.complete_shards_string(), "0");
    }

    #[test]
    fn test_all_shards_sorted() {
        let mut manager = ShardManager::new();
        let hash = Hash256::ZERO;

        manager.import_ledger(2 * LEDGERS_PER_SHARD, hash, vec![1]);
        manager.import_ledger(0, hash, vec![1]);
        manager.import_ledger(LEDGERS_PER_SHARD, hash, vec![1]);

        let shards = manager.all_shards();
        assert_eq!(shards.len(), 3);
        assert_eq!(shards[0].index, 0);
        assert_eq!(shards[1].index, 1);
        assert_eq!(shards[2].index, 2);
    }

    #[test]
    fn test_shard_store_get() {
        let mut store = ShardStore::new();
        store.put(0, 5, vec![1, 2, 3]);
        assert_eq!(store.get(0, 5), Some(&[1u8, 2, 3][..]));
        assert_eq!(store.get(0, 6), None);
        assert_eq!(store.get(1, 5), None);
    }
}
