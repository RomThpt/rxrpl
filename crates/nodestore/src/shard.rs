//! Ledger history sharding.
//!
//! Splits the full ledger history into fixed-size shards of `LEDGERS_PER_SHARD`
//! consecutive ledgers. Each shard is identified by an index and can be stored,
//! downloaded, and queried independently.

use std::collections::HashMap;
use std::fmt;

use rxrpl_ledger::header::LedgerHeader;
use rxrpl_primitives::Hash256;
use rxrpl_storage::KvStore;

/// Errors that can occur during shard verification.
#[derive(Debug)]
pub enum ShardVerifyError {
    /// The shard is not yet complete.
    NotComplete(u32),
    /// Missing ledger data for the given sequence in the given shard.
    MissingData(u32, u32),
    /// Failed to parse ledger header at the given sequence.
    ParseError(u32),
}

impl fmt::Display for ShardVerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotComplete(idx) => write!(f, "shard {idx} is not complete"),
            Self::MissingData(seq, idx) => {
                write!(f, "missing ledger data for seq {seq} in shard {idx}")
            }
            Self::ParseError(seq) => write!(f, "failed to parse ledger header at seq {seq}"),
        }
    }
}

impl std::error::Error for ShardVerifyError {}

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

/// Persistent shard data store backed by a `KvStore`.
///
/// Key format:
///   - `shard:{index}:{seq}` -> raw ledger data bytes
///   - `shard_meta:{index}` -> `ShardInfo` serialized as JSON
pub struct PersistentShardStore<S: KvStore> {
    store: S,
}

impl<S: KvStore> PersistentShardStore<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    fn data_key(shard_index: u32, seq: u32) -> Vec<u8> {
        format!("shard:{shard_index}:{seq}").into_bytes()
    }

    fn meta_key(shard_index: u32) -> Vec<u8> {
        format!("shard_meta:{shard_index}").into_bytes()
    }

    /// Insert ledger data for a specific shard and sequence.
    pub fn put(&self, shard_index: u32, seq: u32, data: Vec<u8>) -> Result<(), String> {
        self.store
            .put(&Self::data_key(shard_index, seq), &data)
            .map_err(|e| e.to_string())
    }

    /// Retrieve ledger data for a specific shard and sequence.
    pub fn get(&self, shard_index: u32, seq: u32) -> Result<Option<Vec<u8>>, String> {
        self.store
            .get(&Self::data_key(shard_index, seq))
            .map_err(|e| e.to_string())
    }

    /// Check if a specific ledger entry exists.
    pub fn exists(&self, shard_index: u32, seq: u32) -> Result<bool, String> {
        self.store
            .exists(&Self::data_key(shard_index, seq))
            .map_err(|e| e.to_string())
    }

    /// Store shard metadata as JSON.
    pub fn put_meta(&self, info: &ShardInfo) -> Result<(), String> {
        let state_str = match &info.state {
            ShardState::Complete => "complete".to_string(),
            ShardState::Incomplete { stored_count } => format!("incomplete:{stored_count}"),
            ShardState::Downloading => "downloading".to_string(),
        };
        let hash_str = info
            .last_hash
            .map(|h| h.to_string())
            .unwrap_or_default();
        let meta = format!(
            "{}:{}:{}:{}:{}",
            info.index, state_str, info.first_seq, info.last_seq, hash_str
        );
        self.store
            .put(&Self::meta_key(info.index), meta.as_bytes())
            .map_err(|e| e.to_string())
    }

    /// Retrieve shard metadata.
    pub fn get_meta(&self, shard_index: u32) -> Result<Option<ShardInfo>, String> {
        match self.store.get(&Self::meta_key(shard_index)) {
            Ok(Some(bytes)) => {
                let s = String::from_utf8(bytes).map_err(|e| e.to_string())?;
                parse_shard_meta(&s).map(Some)
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Parse the serialized metadata string back into a `ShardInfo`.
fn parse_shard_meta(s: &str) -> Result<ShardInfo, String> {
    let parts: Vec<&str> = s.splitn(5, ':').collect();
    if parts.len() < 4 {
        return Err("invalid shard meta format".into());
    }

    let index: u32 = parts[0].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;

    let state = if parts[1] == "complete" {
        ShardState::Complete
    } else if parts[1] == "downloading" {
        ShardState::Downloading
    } else if let Some(count_str) = parts[1].strip_prefix("incomplete:") {
        let stored_count: u32 = count_str.parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
        ShardState::Incomplete { stored_count }
    } else {
        // Handle the case where the state field itself contains a colon
        // and the count is in the next part
        if parts[1] == "incomplete" && parts.len() >= 5 {
            let stored_count: u32 = parts[2].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
            // Re-parse with adjusted positions
            let first_seq: u32 = parts[3].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
            let rest = if parts.len() > 4 { parts[4] } else { "" };
            let rest_parts: Vec<&str> = rest.splitn(2, ':').collect();
            let last_seq: u32 = rest_parts[0].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
            let last_hash = if rest_parts.len() > 1 && !rest_parts[1].is_empty() {
                rest_parts[1].parse::<Hash256>().ok()
            } else {
                None
            };
            return Ok(ShardInfo {
                index,
                state: ShardState::Incomplete { stored_count },
                first_seq,
                last_seq,
                last_hash,
            });
        }
        return Err(format!("unknown shard state: {}", parts[1]));
    };

    let first_seq: u32 = parts[2].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let last_seq: u32 = parts[3].parse().map_err(|e: std::num::ParseIntError| e.to_string())?;
    let last_hash = if parts.len() > 4 && !parts[4].is_empty() {
        parts[4].parse::<Hash256>().ok()
    } else {
        None
    };

    Ok(ShardInfo {
        index,
        state,
        first_seq,
        last_seq,
        last_hash,
    })
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

    /// Return indices of all complete shards.
    pub fn complete_shard_indices(&self) -> Vec<u32> {
        let mut indices: Vec<u32> = self
            .shards
            .values()
            .filter(|s| s.state == ShardState::Complete)
            .map(|s| s.index)
            .collect();
        indices.sort();
        indices
    }

    /// Return `(index, stored_count)` pairs for all incomplete shards.
    pub fn incomplete_shard_info(&self) -> Vec<(u32, u32)> {
        let mut result: Vec<(u32, u32)> = self
            .shards
            .values()
            .filter_map(|s| match s.state {
                ShardState::Incomplete { stored_count } => Some((s.index, stored_count)),
                _ => None,
            })
            .collect();
        result.sort_by_key(|(idx, _)| *idx);
        result
    }

    /// Retrieve raw ledger data from the store.
    pub fn get_ledger_data(&self, shard_index: u32, seq: u32) -> Option<&[u8]> {
        self.store.get(shard_index, seq)
    }

    /// Verify the hash chain continuity of a complete shard.
    ///
    /// For each consecutive pair of ledgers `(seq, seq+1)` in the shard,
    /// verifies that `header[seq+1].parent_hash == header[seq].hash`.
    ///
    /// Returns `Ok(true)` if the chain is valid, `Ok(false)` if broken.
    pub fn verify_shard(&self, index: u32) -> Result<bool, ShardVerifyError> {
        if !self.is_complete(index) {
            return Err(ShardVerifyError::NotComplete(index));
        }

        let (first_seq, last_seq) = shard_range(index);

        // Parse first header
        let first_data = self
            .store
            .get(index, first_seq)
            .ok_or(ShardVerifyError::MissingData(first_seq, index))?;
        let mut prev_header =
            LedgerHeader::from_raw_bytes(first_data).ok_or(ShardVerifyError::ParseError(first_seq))?;

        // Walk the chain
        for seq in (first_seq + 1)..=last_seq {
            let data = self
                .store
                .get(index, seq)
                .ok_or(ShardVerifyError::MissingData(seq, index))?;
            let header =
                LedgerHeader::from_raw_bytes(data).ok_or(ShardVerifyError::ParseError(seq))?;

            if header.parent_hash != prev_header.hash {
                return Ok(false);
            }
            prev_header = header;
        }

        Ok(true)
    }

    /// Check whether a shard is currently being downloaded.
    pub fn is_downloading(&self, index: u32) -> bool {
        self.shards
            .get(&index)
            .map(|s| s.state == ShardState::Downloading)
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
    use rxrpl_ledger::header::{RAW_HEADER_SIZE, INITIAL_XRP_DROPS};

    /// Serialize a LedgerHeader into the raw binary format (118 bytes).
    fn header_to_raw(h: &LedgerHeader) -> Vec<u8> {
        let mut buf = Vec::with_capacity(RAW_HEADER_SIZE);
        buf.extend_from_slice(&h.sequence.to_be_bytes());
        buf.extend_from_slice(&h.drops.to_be_bytes());
        buf.extend_from_slice(h.parent_hash.as_bytes());
        buf.extend_from_slice(h.tx_hash.as_bytes());
        buf.extend_from_slice(h.account_hash.as_bytes());
        buf.extend_from_slice(&h.parent_close_time.to_be_bytes());
        buf.extend_from_slice(&h.close_time.to_be_bytes());
        buf.push(h.close_time_resolution);
        buf.push(h.close_flags);
        buf
    }

    /// Build a chain of `count` linked ledger headers starting at `first_seq`.
    /// Returns Vec of (sequence, hash, raw_bytes).
    fn build_header_chain(first_seq: u32, count: u32) -> Vec<(u32, Hash256, Vec<u8>)> {
        let mut chain = Vec::with_capacity(count as usize);
        let mut parent_hash = Hash256::ZERO;

        for i in 0..count {
            let mut h = LedgerHeader::new();
            h.sequence = first_seq + i;
            h.drops = INITIAL_XRP_DROPS;
            h.parent_hash = parent_hash;
            h.close_time = 1000 + i;
            h.parent_close_time = if i == 0 { 0 } else { 999 + i };
            h.close_time_resolution = 30;
            h.hash = h.compute_hash();

            let raw = header_to_raw(&h);
            parent_hash = h.hash;
            chain.push((h.sequence, h.hash, raw));
        }

        chain
    }

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

    /// Use a small shard size for verification tests to keep them fast.
    /// We test with a "mini shard" by filling exactly LEDGERS_PER_SHARD entries.
    /// Since that's 16384 entries, we test verify_shard logic with a smaller
    /// manual approach instead.

    #[test]
    fn test_verify_shard_incomplete() {
        let mut manager = ShardManager::new();
        manager.import_ledger(0, Hash256::ZERO, vec![0u8; 32]);
        // Shard 0 is incomplete
        assert!(matches!(
            manager.verify_shard(0),
            Err(ShardVerifyError::NotComplete(0))
        ));
    }

    #[test]
    fn test_verify_shard_nonexistent() {
        let manager = ShardManager::new();
        // Shard 99 doesn't exist, so is_complete returns false
        assert!(matches!(
            manager.verify_shard(99),
            Err(ShardVerifyError::NotComplete(99))
        ));
    }

    // Use shard 1 (seq LEDGERS_PER_SHARD..2*LEDGERS_PER_SHARD) for verification
    // tests because LedgerHeader::from_raw_bytes rejects sequence 0.
    const TEST_SHARD_INDEX: u32 = 1;
    const TEST_FIRST_SEQ: u32 = LEDGERS_PER_SHARD;

    #[test]
    fn test_verify_shard_valid_chain() {
        let mut manager = ShardManager::new();
        let chain = build_header_chain(TEST_FIRST_SEQ, LEDGERS_PER_SHARD);

        for (seq, hash, data) in &chain {
            manager.import_ledger(*seq, *hash, data.clone());
        }

        assert!(manager.is_complete(TEST_SHARD_INDEX));
        assert_eq!(manager.verify_shard(TEST_SHARD_INDEX).unwrap(), true);
    }

    #[test]
    fn test_verify_shard_broken_chain() {
        let mut manager = ShardManager::new();
        let mut chain = build_header_chain(TEST_FIRST_SEQ, LEDGERS_PER_SHARD);

        // Corrupt the parent_hash of the 10th entry
        let corrupt_idx = 10usize;
        let corrupt_seq = TEST_FIRST_SEQ + corrupt_idx as u32;
        let mut bad_header = LedgerHeader::new();
        bad_header.sequence = corrupt_seq;
        bad_header.drops = INITIAL_XRP_DROPS;
        bad_header.parent_hash = Hash256::new([0xFF; 32]); // wrong parent
        bad_header.close_time = 1000 + corrupt_idx as u32;
        bad_header.parent_close_time = 999 + corrupt_idx as u32;
        bad_header.close_time_resolution = 30;
        bad_header.hash = bad_header.compute_hash();
        chain[corrupt_idx] = (corrupt_seq, bad_header.hash, header_to_raw(&bad_header));

        for (seq, hash, data) in &chain {
            manager.import_ledger(*seq, *hash, data.clone());
        }

        assert!(manager.is_complete(TEST_SHARD_INDEX));
        assert_eq!(manager.verify_shard(TEST_SHARD_INDEX).unwrap(), false);
    }

    #[test]
    fn test_verify_shard_parse_error() {
        let mut manager = ShardManager::new();
        let chain = build_header_chain(TEST_FIRST_SEQ, LEDGERS_PER_SHARD);
        let garbage_seq = TEST_FIRST_SEQ + 5;

        for (seq, hash, data) in &chain {
            if *seq == garbage_seq {
                // Store garbage data that can't be parsed
                manager.import_ledger(*seq, *hash, vec![0xDE; 50]);
            } else {
                manager.import_ledger(*seq, *hash, data.clone());
            }
        }

        assert!(manager.is_complete(TEST_SHARD_INDEX));
        assert!(matches!(
            manager.verify_shard(TEST_SHARD_INDEX),
            Err(ShardVerifyError::ParseError(seq)) if seq == garbage_seq
        ));
    }
}
