use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rxrpl_primitives::Hash256;
use rxrpl_shamap::{LeafNode, NodeStore, SHAMap};

const MAX_CONCURRENT_REQUESTS: usize = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum number of missing node hashes to request in a single delta sync round.
const MAX_DELTA_NODES_PER_REQUEST: usize = 128;
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
}

/// Tracks in-flight ledger sync requests and manages retries/timeouts.
pub struct LedgerSyncer {
    pending: HashMap<u32, PendingRequest>,
    max_concurrent: usize,
    timeout: Duration,
    /// Active incremental syncs keyed by ledger sequence.
    incremental: HashMap<u32, IncrementalSync>,
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

    /// Clear all pending requests (e.g., on full sync reset).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.incremental.clear();
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
    ) -> Vec<Hash256> {
        let entry = self.incremental.entry(seq).or_insert_with(|| {
            let map = SHAMap::syncing_with_store(hash, LeafNode::account_state, store);
            IncrementalSync {
                hash,
                map,
                rounds: 0,
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
    /// Returns `true` if the sync is now complete (all nodes resolved).
    pub fn feed_nodes(
        &mut self,
        seq: u32,
        nodes: &[(Vec<u8>, Vec<u8>)],
    ) -> bool {
        let entry = match self.incremental.get_mut(&seq) {
            Some(e) => e,
            None => return false,
        };

        let mut added = 0;
        for (node_id, node_data) in nodes {
            if node_id.len() >= 32 {
                let hash_bytes: [u8; 32] = node_id[..32].try_into().unwrap_or([0u8; 32]);
                let hash = Hash256::new(hash_bytes);
                match entry.map.add_raw_node(hash, node_data.clone()) {
                    Ok(true) => added += 1,
                    Ok(false) => {}
                    Err(e) => {
                        tracing::debug!(
                            "failed to add raw node {} for ledger #{}: {}",
                            hash, seq, e
                        );
                    }
                }
            }
        }

        if added > 0 {
            // Reload root from the store in case the root node was among the
            // received nodes.
            let _ = entry.map.reload_root(entry.hash);
        }

        // Check if the tree is now complete.
        if entry.map.is_complete() {
            tracing::info!(
                "incremental sync for ledger #{} complete after {} rounds",
                seq,
                entry.rounds
            );
            self.incremental.remove(&seq);
            true
        } else {
            false
        }
    }

    /// Get the missing node hashes for an active incremental sync, if any.
    ///
    /// Called by `send_get_ledger` to populate `node_ids` in the request.
    pub fn get_missing_node_ids(&self, seq: u32) -> Vec<Hash256> {
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
