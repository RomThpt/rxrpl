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

/// Data received from a synced ledger.
pub struct SyncedLedgerData {
    pub seq: u32,
    pub hash: Hash256,
    pub nodes: Vec<(Vec<u8>, Vec<u8>)>,
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
        if !self.pending.contains_key(&seq) {
            self.pending.insert(
                seq,
                PendingRequest {
                    hash,
                    sent_at: Instant::now(),
                    retries: 0,
                },
            );
        }
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
            if !self.pending.contains_key(&seq) {
                self.pending.insert(
                    seq,
                    PendingRequest {
                        hash: None,
                        sent_at: Instant::now(),
                        retries: 0,
                    },
                );
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
        // Only sync one ledger at a time. Replace the active sync with
        // a newer ledger to avoid syncing stale data. The store retains
        // all previously fetched nodes, so the new sync picks up where
        // the old one left off.
        if !self.incremental.contains_key(&seq) {
            if let Some(&active_seq) = self.incremental.keys().max() {
                if seq <= active_seq {
                    return Vec::new();
                }
                // Replace when the active sync is stuck (zero-add rounds).
                // Don't replace during active progress to avoid thrashing.
                let zero_rounds = self.incremental.get(&active_seq)
                    .map(|e| e.zero_rounds)
                    .unwrap_or(0);
                if zero_rounds < 8 {
                    return Vec::new();
                }
                tracing::info!(
                    "replacing stale sync #{} (round {}) with #{}",
                    active_seq, zero_rounds, seq
                );
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
    /// Returns `Some(leaf_nodes)` if the sync is now complete (all nodes resolved),
    /// where `leaf_nodes` are the (key, data) pairs from the completed SHAMap.
    /// Returns `None` if the sync is still in progress or no sync is active.
    pub fn feed_nodes(
        &mut self,
        seq: u32,
        nodes: &[(Vec<u8>, Vec<u8>)],
    ) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
        let entry = match self.incremental.get_mut(&seq) {
            Some(e) => e,
            None => return None,
        };

        let mut added = 0;
        for (_node_id, node_data) in nodes {
            // rippled sends nodedata as: serialized_node_bytes + depth_byte.
            // Strip the trailing depth byte to get pure node data.
            // Then compute the content hash to store it correctly.
            if node_data.is_empty() {
                continue;
            }
            let raw = if node_data.len() == 513 || (node_data.len() > 32 && node_data.len() % 32 != 0) {
                // Strip trailing depth byte from rippled's SHAMap wire format.
                &node_data[..node_data.len() - 1]
            } else {
                &node_data[..]
            };

            // Compute the content hash based on node type.
            let hash = if raw.len() == 512 {
                // Inner node: hash = SHA-512-Half("MIN\0" || raw)
                let prefix: [u8; 4] = [0x4D, 0x49, 0x4E, 0x00]; // "MIN\0"
                rxrpl_crypto::sha512_half::sha512_half(&[&prefix, raw])
            } else if raw.len() >= 32 {
                // Leaf node: hash = SHA-512-Half("MLN\0" || raw)
                // Wire format is key(32) || data; hash covers both in that order.
                let prefix: [u8; 4] = [0x4D, 0x4C, 0x4E, 0x00]; // "MLN\0"
                rxrpl_crypto::sha512_half::sha512_half(&[&prefix, raw])
            } else {
                // Too small, skip.
                continue;
            };

            tracing::debug!(
                "feed_nodes #{}: computed hash={} size={} bytes (stripped from {})",
                seq, hash, raw.len(), node_data.len()
            );

            match entry.map.add_raw_node(hash, raw.to_vec()) {
                Ok(true) => added += 1,
                Ok(false) => {}
                Err(e) => {
                    tracing::debug!(
                        "feed_nodes #{}: failed to add node {} ({} bytes): {}",
                        seq, hash, raw.len(), e
                    );
                }
            }
        }

        tracing::debug!(
            "feed_nodes #{}: added {} new nodes out of {} received",
            seq, added, nodes.len()
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
                    seq, entry.hash, e
                );
            }
        } else {
            entry.zero_rounds += 1;
            if entry.zero_rounds > 20 {
                tracing::warn!(
                    "incremental sync for ledger #{} stuck ({} consecutive zero-add rounds), removing",
                    seq, entry.zero_rounds
                );
                self.incremental.remove(&seq);
                return None;
            }
        }

        // A tree with an empty root is never complete (it needs the root first).
        if entry.map.is_empty() {
            return None;
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
            Some(leaves)
        } else {
            None
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
}
