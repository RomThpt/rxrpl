use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rxrpl_primitives::Hash256;
use rxrpl_shamap::{LeafNode, MissingNode, NodeStore, SHAMap};

const MAX_CONCURRENT_REQUESTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum number of missing node hashes to request in a single delta sync round.
const MAX_DELTA_NODES_PER_REQUEST: usize = 512;
/// Maximum number of sync rounds before giving up on incremental sync.
const MAX_INCREMENTAL_ROUNDS: u32 = 50;
/// Number of consecutive zero-add rounds before falling back to hash-based fetch.
const HASH_FALLBACK_THRESHOLD: u32 = 5;
/// Consecutive zero-add rounds before a higher tip seq may preempt the active
/// sync during normal tip-following (we already hold a base state).
const TIP_PREEMPT_ZERO_ROUNDS: u32 = 8;
/// Consecutive zero-add rounds before the LOCKED initial-catchup target is
/// considered dead (its state has aged out of every peer) and may be replaced
/// by a newer tip. Far higher than `TIP_PREEMPT_ZERO_ROUNDS` so a large state
/// fetch is not reset mid-flight by the ~4s-moving mainnet tip.
const CATCHUP_PREEMPT_ZERO_ROUNDS: u32 = 40;
/// Consecutive zero-add rounds before an active sync is abandoned outright.
/// Normal tip-following uses the lower bound; the locked initial-catchup target
/// uses the higher one so preemption by a newer tip (above) stays the primary
/// release path and removal is only a backstop when no newer tip arrives.
const STUCK_REMOVE_ZERO_ROUNDS: u32 = 20;
const CATCHUP_STUCK_REMOVE_ZERO_ROUNDS: u32 = 48;

/// Data received from a synced ledger.
pub struct SyncedLedgerData {
    pub seq: u32,
    pub hash: Hash256,
    pub nodes: Vec<(Vec<u8>, Vec<u8>)>,
}

/// rippled `SHAMapTreeNode::wireType*` constants.
/// See `include/xrpl/shamap/SHAMapTreeNode.h:17-21`.
const WIRE_TYPE_TRANSACTION: u8 = 0;
const WIRE_TYPE_ACCOUNT_STATE: u8 = 1;
const WIRE_TYPE_INNER: u8 = 2;
const WIRE_TYPE_COMPRESSED_INNER: u8 = 3;
const WIRE_TYPE_TRANSACTION_WITH_META: u8 = 4;

const HASH_PREFIX_INNER: [u8; 4] = [b'M', b'I', b'N', 0]; // HashPrefix::innerNode
const HASH_PREFIX_LEAF: [u8; 4] = [b'M', b'L', b'N', 0]; // HashPrefix::leafNode (account state)
const HASH_PREFIX_TX_NODE: [u8; 4] = [b'S', b'N', b'D', 0]; // HashPrefix::txNode (tx with meta)
const HASH_PREFIX_TX_ID: [u8; 4] = [b'T', b'X', b'N', 0]; // HashPrefix::transactionID

/// Decode a rippled-format SHAMap wire node into (content_hash, storage_bytes).
///
/// Wire layout (rippled `SHAMapTreeNode::makeFromWire`):
/// - Inner full (wireType=2): `16 * 32 bytes child hashes || 0x02`
/// - Inner compressed (wireType=3): `N * (hash[32] || branch[1]) || 0x03`
/// - Leaf account state (wireType=1): `data || key[32] || 0x01`
/// - Leaf tx with meta (wireType=4): `data || key[32] || 0x04`
/// - Leaf tx no meta (wireType=0): `data || 0x00`
///
/// Storage format expected by `crates/shamap/src/node_store.rs::deserialize_node`:
/// - Inner: `16 * 32 bytes` (no prefix, no trailing byte)
/// - Leaf: `key[32] || data` (rxrpl always prepends the key)
///
/// Hash format (matches rippled `serializeWithPrefix`, post-PR #30):
/// - Inner: `SHA512Half(HASH_PREFIX_INNER || 16*32 child hashes)`
/// - Leaf account state: `SHA512Half(HASH_PREFIX_LEAF || data || key)`
/// - Leaf tx with meta: `SHA512Half(HASH_PREFIX_TX_NODE || data || key)`
/// - Leaf tx no meta: `SHA512Half(HASH_PREFIX_TX_ID || data)`
fn decode_wire_node(node_data: &[u8]) -> Option<(Hash256, Vec<u8>)> {
    if node_data.len() < 2 {
        return None;
    }
    let wire_type = node_data[node_data.len() - 1];
    let payload = &node_data[..node_data.len() - 1];

    match wire_type {
        WIRE_TYPE_INNER => {
            if payload.len() != 16 * 32 {
                return None;
            }
            let hash = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_INNER, payload]);
            Some((hash, payload.to_vec()))
        }
        WIRE_TYPE_COMPRESSED_INNER => {
            // N * (hash[32] || branch[1])
            if payload.is_empty() || payload.len() % 33 != 0 {
                return None;
            }
            let mut full = vec![0u8; 16 * 32];
            for chunk in payload.chunks_exact(33) {
                let branch = chunk[32] as usize;
                if branch >= 16 {
                    return None;
                }
                full[branch * 32..(branch + 1) * 32].copy_from_slice(&chunk[..32]);
            }
            let hash = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_INNER, &full]);
            Some((hash, full))
        }
        WIRE_TYPE_ACCOUNT_STATE => {
            // payload = data || key[32]
            if payload.len() < 32 {
                return None;
            }
            let split = payload.len() - 32;
            let data = &payload[..split];
            let key = &payload[split..];
            let hash = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_LEAF, data, key]);
            // Convert wire layout (data || key) to storage layout (key || data).
            let mut storage = Vec::with_capacity(payload.len());
            storage.extend_from_slice(key);
            storage.extend_from_slice(data);
            Some((hash, storage))
        }
        WIRE_TYPE_TRANSACTION_WITH_META => {
            // payload = data || key[32]
            if payload.len() < 32 {
                return None;
            }
            let split = payload.len() - 32;
            let data = &payload[..split];
            let key = &payload[split..];
            let hash = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_TX_NODE, data, key]);
            let mut storage = Vec::with_capacity(payload.len());
            storage.extend_from_slice(key);
            storage.extend_from_slice(data);
            Some((hash, storage))
        }
        WIRE_TYPE_TRANSACTION => {
            // payload = data only; key = SHA512Half(TXN || data) i.e. the tx hash
            if payload.is_empty() {
                return None;
            }
            let key = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_TX_ID, payload]);
            // For tx-no-meta, key IS the hash. Storage = key || data.
            let mut storage = Vec::with_capacity(32 + payload.len());
            storage.extend_from_slice(key.as_bytes());
            storage.extend_from_slice(payload);
            Some((key, storage))
        }
        _ => None,
    }
}

/// Convert a `TMGetObjectByHash` NodeObject blob into the SHAMap wire form
/// that `decode_wire_node` expects.
///
/// `GetObjectByHash` returns `serializeWithPrefix` blobs shaped as a 4-byte
/// `HASH_PREFIX` followed by the node content. `GetLedger` / `TMLedgerData` —
/// what `decode_wire_node` parses — instead puts the content first and a single
/// wire-type byte last. The prefix identifies the node type, so this maps that
/// prefix to the matching trailing byte and moves it from the front to the
/// back. The recomputed `SHA512Half(HASH_PREFIX, content)` then matches the
/// object's real hash, which is why misparsing the prefix blob as a wire node
/// produced wrong hashes and left mainnet catchup stuck.
pub fn object_blob_to_wire(blob: &[u8]) -> Option<Vec<u8>> {
    if blob.len() < 4 {
        return None;
    }
    let prefix: [u8; 4] = blob[..4].try_into().ok()?;
    let content = &blob[4..];
    let wire_type = match prefix {
        HASH_PREFIX_INNER => WIRE_TYPE_INNER,
        HASH_PREFIX_LEAF => WIRE_TYPE_ACCOUNT_STATE,
        HASH_PREFIX_TX_NODE => WIRE_TYPE_TRANSACTION_WITH_META,
        HASH_PREFIX_TX_ID => WIRE_TYPE_TRANSACTION,
        _ => return None,
    };
    let mut wire = Vec::with_capacity(content.len() + 1);
    wire.extend_from_slice(content);
    wire.push(wire_type);
    Some(wire)
}

/// Result of feeding nodes into an incremental sync.
pub enum FeedResult {
    /// Sync is still in progress; continue with tree-based requests.
    Continue,
    /// Sync is complete; contains the extracted leaf nodes.
    Complete(Vec<(Vec<u8>, Vec<u8>)>),
    /// Tree-based sync is stuck after repeated zero-add rounds.
    /// Contains the content hashes of missing nodes for hash-based fallback.
    FallbackToHashFetch(Vec<Hash256>),
    /// Sync was removed (gave up or not found).
    Removed,
}

/// A ledger being synced incrementally via delta sync.
struct IncrementalSync {
    hash: Hash256,
    map: SHAMap,
    rounds: u32,
    zero_rounds: u32,
    total_added: u32,
}

/// Tracks in-flight ledger sync requests and manages retries/timeouts.
pub struct LedgerSyncer {
    pending: HashMap<u32, PendingRequest>,
    max_concurrent: usize,
    timeout: Duration,
    /// Active incremental syncs keyed by ledger sequence.
    incremental: HashMap<u32, IncrementalSync>,
    /// Mapping of seq -> ledger hash for liAS_NODE follow-up requests.
    ledger_hashes: HashMap<u32, Hash256>,
    /// Sequences that have already been synced (to avoid re-processing).
    synced_seqs: HashSet<u32>,
}

struct PendingRequest {
    /// Expected ledger hash, captured at request time so future retry/validation
    /// logic can correlate responses. Not yet read by the syncer.
    #[allow(dead_code)]
    hash: Option<Hash256>,
    sent_at: Instant,
    retries: u32,
}

impl LedgerSyncer {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
            max_concurrent: MAX_CONCURRENT_REQUESTS,
            timeout: REQUEST_TIMEOUT,
            incremental: HashMap::new(),
            ledger_hashes: HashMap::new(),
            synced_seqs: HashSet::new(),
        }
    }

    /// Register an outgoing ledger request so that responses can be correlated.
    ///
    /// Called by `PeerManager::send_get_ledger` to ensure the response handler
    /// can match incoming `LedgerData` to a pending request.
    pub fn register_request(&mut self, seq: u32, hash: Option<Hash256>) {
        self.pending.entry(seq).or_insert_with(|| PendingRequest {
            hash,
            sent_at: Instant::now(),
            retries: 0,
        });
    }

    /// Check if we need to sync based on our sequence vs a peer's sequence.
    pub fn needs_sync(&self, our_seq: u32, peer_seq: u32) -> bool {
        peer_seq > our_seq + 1
    }

    /// Generate a list of (seq, optional_hash) pairs to request.
    ///
    /// Returns at most `max_concurrent` outstanding requests.
    pub fn request_missing(
        &mut self,
        our_seq: u32,
        target_seq: u32,
    ) -> Vec<(u32, Option<Hash256>)> {
        let mut requests = Vec::new();
        let mut seq = our_seq + 1;

        while seq < target_seq && self.pending.len() < self.max_concurrent {
            if let std::collections::hash_map::Entry::Vacant(e) = self.pending.entry(seq) {
                e.insert(PendingRequest {
                    hash: None,
                    sent_at: Instant::now(),
                    retries: 0,
                });
                requests.push((seq, None));
            }
            seq += 1;
        }

        requests
    }

    /// Handle a ledger data response. Returns `Some(SyncedLedgerData)` if valid.
    pub fn handle_response(
        &mut self,
        seq: u32,
        hash: Hash256,
        nodes: Vec<(Vec<u8>, Vec<u8>)>,
    ) -> Option<SyncedLedgerData> {
        if self.pending.remove(&seq).is_some() {
            Some(SyncedLedgerData { seq, hash, nodes })
        } else {
            None
        }
    }

    /// Check for timed-out requests and return their sequence numbers for retry.
    pub fn check_timeouts(&mut self, now: Instant) -> Vec<u32> {
        let mut timed_out = Vec::new();

        self.pending.retain(|seq, req| {
            if now.duration_since(req.sent_at) > self.timeout {
                if req.retries >= 3 {
                    // Give up after 3 retries
                    timed_out.push(*seq);
                    return false;
                }
                timed_out.push(*seq);
                req.retries += 1;
                req.sent_at = now;
            }
            true
        });

        timed_out
    }

    /// Number of currently pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Store the ledger hash for a given sequence (used for liAS_NODE requests).
    pub fn set_ledger_hash(&mut self, seq: u32, hash: Hash256) {
        self.ledger_hashes.insert(seq, hash);
    }

    /// Get the stored ledger hash for a given sequence.
    pub fn get_ledger_hash(&self, seq: u32) -> Option<Hash256> {
        self.ledger_hashes.get(&seq).copied()
    }

    /// Clear all pending requests (e.g., on full sync reset).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.incremental.clear();
        self.ledger_hashes.clear();
        self.synced_seqs.clear();
    }

    // --- Incremental (delta) sync ---

    /// Start or continue an incremental sync for a ledger.
    ///
    /// Creates a `SHAMap` in syncing state backed by the provided store,
    /// then returns the hashes of missing nodes that need to be fetched.
    pub fn start_incremental_sync(
        &mut self,
        seq: u32,
        hash: Hash256,
        store: Arc<dyn NodeStore>,
    ) -> Vec<MissingNode> {
        // Only sync one ledger at a time. The store retains all previously
        // fetched nodes, so a replacement sync picks up where the old one
        // left off.
        //
        // A LOWER seq than the active sync always wins the slot: contiguous
        // catchup must reconstruct ledgers in ascending order (each adopted
        // ledger is the prerequisite for the next), so the lowest missing
        // ledger is never stale — it is exactly the one we must finish first.
        // Without this, a concurrently-requested higher-seq tip (from the
        // StatusChange `request_missing` / peer-tip paths) would seize the
        // slot and silently starve every intermediate ledger, leaving holes
        // in `closed_ledgers`.
        //
        // A HIGHER seq only replaces the active sync once that sync is stuck
        // (zero-add rounds), to avoid thrashing during real progress.
        // Initial state catchup = we have never completed a ledger yet. In that
        // mode we lock onto a single target instead of chasing the moving tip.
        let initial_catchup = self.synced_seqs.is_empty();
        if !self.incremental.contains_key(&seq) {
            if let Some(&active_seq) = self.incremental.keys().max() {
                if seq > active_seq {
                    let zero_rounds = self
                        .incremental
                        .get(&active_seq)
                        .map(|e| e.zero_rounds)
                        .unwrap_or(0);
                    let preempt_threshold = if initial_catchup {
                        CATCHUP_PREEMPT_ZERO_ROUNDS
                    } else {
                        TIP_PREEMPT_ZERO_ROUNDS
                    };
                    if zero_rounds < preempt_threshold {
                        return Vec::new();
                    }
                    tracing::info!(
                        "replacing stale sync #{} (round {}) with #{}",
                        active_seq,
                        zero_rounds,
                        seq
                    );
                } else {
                    tracing::info!(
                        "preempting sync #{} with lower contiguous ledger #{}",
                        active_seq,
                        seq
                    );
                }
                self.incremental.clear();
            }
        }

        let entry = self.incremental.entry(seq).or_insert_with(|| {
            let map = SHAMap::syncing_with_store(hash, LeafNode::account_state, store);
            IncrementalSync {
                hash,
                map,
                rounds: 0,
                zero_rounds: 0,
                total_added: 0,
            }
        });

        entry.rounds += 1;

        if entry.rounds > MAX_INCREMENTAL_ROUNDS {
            tracing::warn!(
                "incremental sync for ledger #{} exceeded {} rounds, giving up",
                seq,
                MAX_INCREMENTAL_ROUNDS
            );
            self.incremental.remove(&seq);
            return Vec::new();
        }

        entry.map.missing_nodes(hash, MAX_DELTA_NODES_PER_REQUEST)
    }

    /// Feed received nodes into an active incremental sync.
    ///
    /// Returns a `FeedResult` indicating the sync state:
    /// - `Complete(leaves)` if the sync finished (all nodes resolved).
    /// - `FallbackToHashFetch(hashes)` if the tree-based sync is stuck and
    ///   should be retried via TMGetObjectByHash with content hashes.
    /// - `Continue` if the sync is still in progress.
    /// - `Removed` if the sync was abandoned or not found.
    pub fn feed_nodes(&mut self, seq: u32, nodes: &[(Vec<u8>, Vec<u8>)]) -> FeedResult {
        // Read before borrowing `incremental`: a locked initial-catchup target
        // (nothing synced yet) survives far more zero-add rounds than a normal
        // tip-following sync before being abandoned.
        let stuck_remove_threshold = if self.synced_seqs.is_empty() {
            CATCHUP_STUCK_REMOVE_ZERO_ROUNDS
        } else {
            STUCK_REMOVE_ZERO_ROUNDS
        };
        let entry = match self.incremental.get_mut(&seq) {
            Some(e) => e,
            None => return FeedResult::Removed,
        };

        let mut added = 0;
        for (_node_id, node_data) in nodes {
            let Some((hash, storage_bytes)) = decode_wire_node(node_data) else {
                continue;
            };

            tracing::debug!(
                "feed_nodes #{}: computed hash={} storage_size={} wire_size={}",
                seq,
                hash,
                storage_bytes.len(),
                node_data.len()
            );

            match entry.map.add_raw_node(hash, storage_bytes) {
                Ok(true) => added += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!("feed_nodes #{}: failed to add node {}: {}", seq, hash, e);
                }
            }
        }

        tracing::debug!(
            "feed_nodes #{}: added {} new nodes out of {} received",
            seq,
            added,
            nodes.len()
        );

        if added > 0 {
            entry.zero_rounds = 0;
            entry.total_added += added as u32;
            if entry.total_added % 5000 < added as u32 {
                tracing::info!("sync #{}: {} total nodes in store", seq, entry.total_added);
            }
            // Reload root from the store in case the root node was among the
            // received nodes.
            if let Err(e) = entry.map.reload_root(entry.hash) {
                tracing::warn!(
                    "feed_nodes #{}: reload_root({}) failed: {}",
                    seq,
                    entry.hash,
                    e
                );
            }
        } else {
            entry.zero_rounds += 1;
            if entry.zero_rounds > stuck_remove_threshold {
                tracing::warn!(
                    "incremental sync for ledger #{} stuck ({} consecutive zero-add rounds), removing",
                    seq,
                    entry.zero_rounds
                );
                self.incremental.remove(&seq);
                return FeedResult::Removed;
            }

            // After HASH_FALLBACK_THRESHOLD zero-add rounds, signal a fallback
            // to TMGetObjectByHash using content hashes instead of SHAMap node IDs.
            if entry.zero_rounds >= HASH_FALLBACK_THRESHOLD
                && entry.zero_rounds % HASH_FALLBACK_THRESHOLD == 0
            {
                let missing = entry
                    .map
                    .missing_nodes(entry.hash, MAX_DELTA_NODES_PER_REQUEST);
                if !missing.is_empty() {
                    let content_hashes: Vec<Hash256> = missing.iter().map(|mn| mn.hash).collect();
                    tracing::info!(
                        "sync #{} stuck for {} zero-add rounds, falling back to hash-based fetch ({} hashes)",
                        seq,
                        entry.zero_rounds,
                        content_hashes.len()
                    );
                    return FeedResult::FallbackToHashFetch(content_hashes);
                }
            }
        }

        // A tree with an empty root is never complete (it needs the root first).
        if entry.map.is_empty() {
            return FeedResult::Continue;
        }

        // Check if the tree is now complete.
        if entry.map.is_complete() {
            tracing::info!(
                "incremental sync for ledger #{} complete after {} rounds",
                seq,
                entry.rounds
            );
            // Extract leaf nodes before removing the sync entry.
            let entry = self.incremental.remove(&seq).unwrap();
            let mut leaves = Vec::new();
            entry.map.for_each(&mut |key, data| {
                leaves.push((key.as_bytes().to_vec(), data.to_vec()));
            });
            FeedResult::Complete(leaves)
        } else {
            FeedResult::Continue
        }
    }

    /// Check if an incremental sync is active for the given sequence.
    pub fn has_incremental_sync(&self, seq: u32) -> bool {
        self.incremental.contains_key(&seq)
    }

    /// Check if any incremental sync is active.
    pub fn has_any_incremental_sync(&self) -> bool {
        !self.incremental.is_empty()
    }

    /// Check if a sequence has already been fully synced.
    pub fn is_synced(&self, seq: u32) -> bool {
        self.synced_seqs.contains(&seq)
    }

    /// Mark a sequence as synced.
    pub fn mark_synced(&mut self, seq: u32) {
        self.synced_seqs.insert(seq);
        // Cleanup: keep at most 100 entries.
        if self.synced_seqs.len() > 100 {
            let min_seq = seq.saturating_sub(50);
            self.synced_seqs.retain(|&s| s >= min_seq);
        }
    }

    pub fn latest_known_seq(&self) -> Option<u32> {
        let a = self.ledger_hashes.keys().copied().max();
        let b = self.incremental.keys().copied().max();
        match (a, b) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }

    /// Attempt to complete an incremental sync immediately (no nodes pending).
    ///
    /// Returns Some(leaves) if the syncing SHAMap is complete (all referenced
    /// nodes resolvable from the backing store). Used when start_incremental_sync
    /// returns no missing nodes — the late joiner already has every required
    /// node in its store (e.g. for early ledgers whose state matches genesis).
    pub fn try_complete_sync(&mut self, seq: u32) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
        let entry = self.incremental.get_mut(&seq)?;
        // Reload root from store in case it was added.
        let _ = entry.map.reload_root(entry.hash);
        if entry.map.is_empty() {
            return None;
        }
        if !entry.map.is_complete() {
            return None;
        }
        let entry = self.incremental.remove(&seq).unwrap();
        let mut leaves = Vec::new();
        entry.map.for_each(&mut |key, data| {
            leaves.push((key.as_bytes().to_vec(), data.to_vec()));
        });
        Some(leaves)
    }

    /// Get the missing node hashes for an active incremental sync, if any.
    ///
    /// Called by `send_get_ledger` to populate `node_ids` in the request.
    pub fn get_missing_node_ids(&self, seq: u32) -> Vec<MissingNode> {
        match self.incremental.get(&seq) {
            Some(entry) => entry
                .map
                .missing_nodes(entry.hash, MAX_DELTA_NODES_PER_REQUEST),
            None => Vec::new(),
        }
    }
}

impl Default for LedgerSyncer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_inner_full_round_trips_to_storage() {
        // 16 distinct child hashes
        let mut payload = Vec::with_capacity(16 * 32);
        for i in 0..16u8 {
            let mut h = [0u8; 32];
            h[0] = i;
            payload.extend_from_slice(&h);
        }
        let mut wire = payload.clone();
        wire.push(WIRE_TYPE_INNER);

        let (hash, storage) = decode_wire_node(&wire).expect("decode");
        assert_eq!(storage.len(), 16 * 32);
        assert_eq!(storage, payload);
        let expected_hash = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_INNER, &payload]);
        assert_eq!(hash, expected_hash);
    }

    #[test]
    fn decode_inner_compressed_expands_branches() {
        // Two branches set: index 0 and index 5
        let mut payload = Vec::new();
        let mut h0 = [0u8; 32];
        h0[0] = 0xAA;
        payload.extend_from_slice(&h0);
        payload.push(0); // branch
        let mut h5 = [0u8; 32];
        h5[0] = 0xBB;
        payload.extend_from_slice(&h5);
        payload.push(5); // branch
        let mut wire = payload.clone();
        wire.push(WIRE_TYPE_COMPRESSED_INNER);

        let (hash, storage) = decode_wire_node(&wire).expect("decode");
        assert_eq!(storage.len(), 16 * 32);
        // branch 0 has h0
        assert_eq!(&storage[0..32], &h0);
        // branch 1..5 zero
        assert!(storage[32..5 * 32].iter().all(|&b| b == 0));
        // branch 5 has h5
        assert_eq!(&storage[5 * 32..6 * 32], &h5);
        // branch 6..16 zero
        assert!(storage[6 * 32..].iter().all(|&b| b == 0));
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_INNER, &storage]);
        assert_eq!(hash, expected);
    }

    #[test]
    fn decode_account_state_reorders_data_and_key() {
        let key = [0xAB; 32];
        let data = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let mut wire = data.clone();
        wire.extend_from_slice(&key);
        wire.push(WIRE_TYPE_ACCOUNT_STATE);

        let (hash, storage) = decode_wire_node(&wire).expect("decode");
        // Storage layout = key || data
        assert_eq!(&storage[..32], &key);
        assert_eq!(&storage[32..], &data[..]);
        // Hash uses rippled order (data || key)
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_LEAF, &data, &key]);
        assert_eq!(hash, expected);
    }

    #[test]
    fn decode_tx_with_meta_uses_snd_prefix() {
        let key = [0xCC; 32];
        let data = vec![0x10, 0x20, 0x30];
        let mut wire = data.clone();
        wire.extend_from_slice(&key);
        wire.push(WIRE_TYPE_TRANSACTION_WITH_META);

        let (hash, storage) = decode_wire_node(&wire).expect("decode");
        assert_eq!(&storage[..32], &key);
        assert_eq!(&storage[32..], &data[..]);
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_TX_NODE, &data, &key]);
        assert_eq!(hash, expected);
    }

    #[test]
    fn decode_tx_no_meta_derives_key_from_data() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut wire = data.clone();
        wire.push(WIRE_TYPE_TRANSACTION);

        let (hash, storage) = decode_wire_node(&wire).expect("decode");
        let expected_key = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_TX_ID, &data]);
        // Hash IS the tx hash
        assert_eq!(hash, expected_key);
        // Storage = key || data
        assert_eq!(&storage[..32], expected_key.as_bytes());
        assert_eq!(&storage[32..], &data[..]);
    }

    #[test]
    fn decode_unknown_wire_type_returns_none() {
        let wire = vec![0x00, 0x99]; // wireType 0x99 unknown
        assert!(decode_wire_node(&wire).is_none());
    }

    #[test]
    fn decode_too_short_returns_none() {
        assert!(decode_wire_node(&[]).is_none());
        assert!(decode_wire_node(&[0x01]).is_none());
    }

    #[test]
    fn decode_inner_full_wrong_size_returns_none() {
        let mut wire = vec![0u8; 100];
        wire.push(WIRE_TYPE_INNER);
        assert!(decode_wire_node(&wire).is_none());
    }

    #[test]
    fn decode_inner_compressed_wrong_alignment_returns_none() {
        // 50 bytes is not a multiple of 33
        let mut wire = vec![0u8; 50];
        wire.push(WIRE_TYPE_COMPRESSED_INNER);
        assert!(decode_wire_node(&wire).is_none());
    }

    #[test]
    fn decode_inner_compressed_invalid_branch_returns_none() {
        let mut wire = vec![0u8; 32];
        wire.push(99); // branch >= 16
        wire.push(WIRE_TYPE_COMPRESSED_INNER);
        assert!(decode_wire_node(&wire).is_none());
    }

    #[test]
    fn needs_sync_when_behind() {
        let syncer = LedgerSyncer::new();
        assert!(syncer.needs_sync(5, 10));
        assert!(!syncer.needs_sync(5, 6));
        assert!(!syncer.needs_sync(5, 5));
    }

    #[test]
    fn request_missing_respects_max() {
        let mut syncer = LedgerSyncer::new();
        let requests = syncer.request_missing(0, 100);
        assert_eq!(requests.len(), MAX_CONCURRENT_REQUESTS);
        assert_eq!(syncer.pending_count(), MAX_CONCURRENT_REQUESTS);
    }

    #[test]
    fn handle_response_removes_pending() {
        let mut syncer = LedgerSyncer::new();
        syncer.request_missing(0, 5);
        assert_eq!(syncer.pending_count(), 4);

        let hash = Hash256::new([0xAA; 32]);
        let result = syncer.handle_response(1, hash, vec![]);
        assert!(result.is_some());
        assert_eq!(result.unwrap().seq, 1);
        assert_eq!(syncer.pending_count(), 3);

        // Unknown seq returns None
        let result = syncer.handle_response(99, hash, vec![]);
        assert!(result.is_none());
    }

    #[test]
    fn check_timeouts_retries() {
        let mut syncer = LedgerSyncer::new();
        syncer.request_missing(0, 3);

        // No timeouts yet
        let timed_out = syncer.check_timeouts(Instant::now());
        assert!(timed_out.is_empty());

        // After timeout duration
        let future = Instant::now() + Duration::from_secs(11);
        let timed_out = syncer.check_timeouts(future);
        assert_eq!(timed_out.len(), 2); // seq 1 and 2
        // Requests should still be pending (retry 1)
        assert_eq!(syncer.pending_count(), 2);
    }

    #[test]
    fn clear_removes_all() {
        let mut syncer = LedgerSyncer::new();
        syncer.request_missing(0, 10);
        assert!(syncer.pending_count() > 0);
        syncer.clear();
        assert_eq!(syncer.pending_count(), 0);
    }

    /// A GetObjectByHash inner NodeObject (`MIN\0 || 16*32 child hashes`)
    /// must convert to wire form and decode back to the object's real hash —
    /// the bug that left mainnet catchup stuck (objects rejected as already
    /// present because the prefix blob was misparsed as a trailing-type wire
    /// node).
    #[test]
    fn object_blob_to_wire_inner_roundtrips_hash() {
        let mut child_hashes = vec![0u8; 16 * 32];
        for (i, b) in child_hashes.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let mut blob = HASH_PREFIX_INNER.to_vec();
        blob.extend_from_slice(&child_hashes);
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_INNER, &child_hashes]);

        let wire = object_blob_to_wire(&blob).expect("inner converts");
        assert_eq!(*wire.last().unwrap(), WIRE_TYPE_INNER);
        let (hash, storage) = decode_wire_node(&wire).expect("wire decodes");
        assert_eq!(hash, expected);
        assert_eq!(storage, child_hashes);
    }

    /// A leaf account-state NodeObject (`MLN\0 || data || key`) likewise.
    #[test]
    fn object_blob_to_wire_leaf_roundtrips_hash() {
        let data = vec![0xABu8; 87];
        let key = vec![0xCDu8; 32];
        let mut blob = HASH_PREFIX_LEAF.to_vec();
        blob.extend_from_slice(&data);
        blob.extend_from_slice(&key);
        let expected = rxrpl_crypto::sha512_half::sha512_half(&[&HASH_PREFIX_LEAF, &data, &key]);

        let wire = object_blob_to_wire(&blob).expect("leaf converts");
        assert_eq!(*wire.last().unwrap(), WIRE_TYPE_ACCOUNT_STATE);
        let (hash, storage) = decode_wire_node(&wire).expect("wire decodes");
        assert_eq!(hash, expected);
        // storage layout is key || data.
        assert_eq!(&storage[..32], &key[..]);
        assert_eq!(&storage[32..], &data[..]);
    }

    #[test]
    fn object_blob_to_wire_rejects_unknown_prefix() {
        let blob = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x01];
        assert!(object_blob_to_wire(&blob).is_none());
    }

    /// Build a server-side account-state SHAMap with `count` well-spread items.
    fn build_server_map(count: usize) -> (rxrpl_shamap::SHAMap, Hash256) {
        use rxrpl_shamap::InMemoryNodeStore;
        let store = Arc::new(InMemoryNodeStore::new());
        let mut map = rxrpl_shamap::SHAMap::account_state_with_store(store);
        for i in 0..count {
            // Spread keys uniformly across the tree so it grows several nibbles deep.
            let key = rxrpl_crypto::sha512_half::sha512_half(&[b"key", &(i as u64).to_be_bytes()]);
            // Small payload so storage (key32 || data) is never exactly 512 bytes.
            let data = vec![(i & 0xFF) as u8; 40];
            map.put(key, data).unwrap();
        }
        let root = map.root_hash();
        (map, root)
    }

    /// Serve a single requested node id from the server map, returning the wire
    /// node bytes in the same `content || wireType` layout `decode_wire_node`
    /// parses (mirrors `peer_manager::encode_shamap_wire_node`).
    fn serve_node_inner(
        server: &rxrpl_shamap::SHAMap,
        node_id: rxrpl_shamap::NodeId,
        compress_inner: bool,
    ) -> Option<(Hash256, Vec<u8>)> {
        let (content_hash, storage, is_inner) = server.node_at(node_id)?;
        let wire = if is_inner && compress_inner {
            // rippled-style compressed inner: only non-zero branches, each as
            // (hash[32] || branch[1]), then wireType 3.
            let mut w = Vec::new();
            for branch in 0..16u8 {
                let h = &storage[branch as usize * 32..(branch as usize + 1) * 32];
                if h != [0u8; 32] {
                    w.extend_from_slice(h);
                    w.push(branch);
                }
            }
            w.push(WIRE_TYPE_COMPRESSED_INNER);
            w
        } else if is_inner {
            let mut w = storage.clone();
            w.push(WIRE_TYPE_INNER);
            w
        } else {
            let key = &storage[..32];
            let data = &storage[32..];
            let mut w = Vec::with_capacity(storage.len() + 1);
            w.extend_from_slice(data);
            w.extend_from_slice(key);
            w.push(WIRE_TYPE_ACCOUNT_STATE);
            w
        };
        Some((content_hash, wire))
    }

    /// In-process reproduction of the mainnet AS_NODE delta-sync loop with a
    /// LARGE, multi-level account-state map and a STABLE target. Server and
    /// client are both rxrpl: if the loop cannot complete, the bug is a pure
    /// rxrpl logic bug in missing_nodes/node_at/feed_nodes; if it completes, the
    /// got_in_want=0 mainnet failure is specific to rippled wire interop.
    #[test]
    fn large_state_delta_sync_completes_against_rxrpl_server() {
        run_large_state_delta_sync(false);
    }

    /// Same loop, but the server emits rippled-style COMPRESSED inner nodes
    /// (wireType 3). This is what a real rippled peer sends on mainnet to save
    /// bandwidth, and the path the full-inner test above does not exercise.
    #[test]
    fn large_state_delta_sync_completes_with_compressed_inner_nodes() {
        run_large_state_delta_sync(true);
    }

    /// During initial state catchup (nothing synced yet) a newer tip must NOT
    /// preempt the locked target while it is below the catchup dead threshold,
    /// even past the normal tip-following preemption point. This is the fix for
    /// the moving mainnet tip resetting a large state fetch mid-flight.
    #[test]
    fn initial_catchup_locks_target_against_moving_tip() {
        let store = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());
        let mut syncer = LedgerSyncer::new();
        let missing = syncer.start_incremental_sync(100, Hash256::new([0x11; 32]), store.clone());
        assert!(!missing.is_empty());

        // Pump zero-add rounds past the tip threshold (8) but below the catchup
        // dead threshold (40).
        for _ in 0..12 {
            let _ = syncer.feed_nodes(100, &[]);
        }

        let preempt = syncer.start_incremental_sync(105, Hash256::new([0x22; 32]), store);
        assert!(
            preempt.is_empty(),
            "newer tip must not preempt the locked catchup target"
        );
        assert!(
            syncer.has_incremental_sync(100),
            "catchup target #100 must stay active"
        );
        assert!(!syncer.has_incremental_sync(105));
    }

    /// Once a base state is held (a ledger has synced) we return to responsive
    /// tip-following: a newer tip preempts a stuck sync at the lower threshold.
    #[test]
    fn tip_following_preempts_stuck_sync_after_base_state() {
        let store = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());
        let mut syncer = LedgerSyncer::new();
        syncer.mark_synced(99);
        let _ = syncer.start_incremental_sync(100, Hash256::new([0x11; 32]), store.clone());
        for _ in 0..10 {
            let _ = syncer.feed_nodes(100, &[]);
        }

        let m = syncer.start_incremental_sync(105, Hash256::new([0x22; 32]), store);
        assert!(
            !m.is_empty(),
            "newer tip preempts a stuck sync once a base state is held"
        );
        assert!(syncer.has_incremental_sync(105));
        assert!(!syncer.has_incremental_sync(100));
    }

    fn run_large_state_delta_sync(compress_inner: bool) {
        const COUNT: usize = 4096;
        let (server, server_root) = build_server_map(COUNT);

        let client_store = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());
        let mut syncer = LedgerSyncer::new();
        let seq = 2u32;
        syncer.set_ledger_hash(seq, server_root);

        let mut missing = syncer.start_incremental_sync(seq, server_root, client_store);
        assert!(!missing.is_empty(), "first round must request the root");

        let mut total_requested = 0usize;
        let mut total_matched = 0usize;
        let mut leaves_out: Option<Vec<(Vec<u8>, Vec<u8>)>> = None;

        for round in 0..2000 {
            // Serve every requested node id, recording request<->response hash match.
            let mut served: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(missing.len());
            let mut matched_this_round = 0usize;
            for mn in &missing {
                total_requested += 1;
                if let Some((content_hash, wire)) =
                    serve_node_inner(&server, mn.node_id, compress_inner)
                {
                    if content_hash == mn.hash {
                        matched_this_round += 1;
                        total_matched += 1;
                    }
                    served.push((mn.node_id.to_wire_bytes(), wire));
                }
            }

            // got_in_want: how many served nodes match what we asked for.
            assert!(
                missing.is_empty() || matched_this_round > 0,
                "round {round}: got_in_want=0 (requested {} node ids, none matched by hash)",
                missing.len()
            );

            match syncer.feed_nodes(seq, &served) {
                FeedResult::Complete(leaves) => {
                    leaves_out = Some(leaves);
                    break;
                }
                FeedResult::Continue | FeedResult::FallbackToHashFetch(_) => {
                    missing = syncer.get_missing_node_ids(seq);
                    if missing.is_empty() {
                        if let Some(leaves) = syncer.try_complete_sync(seq) {
                            leaves_out = Some(leaves);
                            break;
                        }
                    }
                }
                FeedResult::Removed => panic!("round {round}: sync removed (gave up)"),
            }
        }

        let leaves = leaves_out.expect("sync must complete within the round budget");
        assert_eq!(
            total_requested, total_matched,
            "every requested node id must be served with a matching hash"
        );
        assert_eq!(leaves.len(), COUNT, "all leaves must be recovered");

        // Every recovered leaf must match the server's value.
        for (key_bytes, data) in &leaves {
            let key = Hash256::new(key_bytes[..32].try_into().unwrap());
            assert_eq!(server.get(&key), Some(data.as_slice()), "leaf {key} mismatch");
        }
    }
}
