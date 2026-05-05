//! Shard exchange protocol: syncer that downloads shards from peers.
//!
//! Follows the same structural patterns as `LedgerSyncer` but manages
//! shard-level data exchange instead of individual ledger sync.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rxrpl_nodestore::ShardManager;
use rxrpl_p2p_proto::shard_msg::{TMGetShardData, TMShardData, TMShards};
use rxrpl_primitives::Hash256;
use tokio::sync::RwLock;

/// Maximum number of concurrent shard downloads.
const DEFAULT_MAX_CONCURRENT: usize = 3;

/// Timeout for a shard data request before retrying.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum number of ledger sequences to request in a single message.
const MAX_SEQS_PER_REQUEST: usize = 64;

/// Maximum retries before giving up on a shard request.
const MAX_RETRIES: u32 = 3;

/// Tracks an active shard download request.
struct ActiveRequest {
    #[allow(dead_code)]
    shard_index: u32,
    peer_id: Hash256,
    sent_at: Instant,
    retries: u32,
}

/// Shard synchronization manager.
///
/// Tracks which peers have which shards, manages download requests,
/// and feeds received data into the `ShardManager`.
pub struct ShardSyncer {
    shard_manager: Arc<RwLock<ShardManager>>,
    /// Maps peer_id -> list of complete shard indices the peer has.
    peer_shards: HashMap<Hash256, Vec<u32>>,
    /// Maps shard_index -> active download request.
    active_requests: HashMap<u32, ActiveRequest>,
    /// Maximum number of parallel downloads.
    max_concurrent: usize,
    /// Shard indices queued for download.
    download_queue: Vec<u32>,
}

impl ShardSyncer {
    pub fn new(shard_manager: Arc<RwLock<ShardManager>>) -> Self {
        Self {
            shard_manager,
            peer_shards: HashMap::new(),
            active_requests: HashMap::new(),
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            download_queue: Vec::new(),
        }
    }

    /// Handle a TMShards message from a peer advertising available shards.
    pub fn on_shards_message(&mut self, peer_id: Hash256, msg: TMShards) {
        tracing::debug!(
            "peer {} advertises {} complete shards, {} incomplete",
            peer_id,
            msg.indices.len(),
            msg.incomplete.len()
        );
        self.peer_shards.insert(peer_id, msg.indices);
    }

    /// Handle received shard data from a peer.
    ///
    /// Returns the number of ledger entries successfully imported.
    pub async fn on_shard_data(&mut self, peer_id: Hash256, msg: TMShardData) -> u32 {
        let shard_index = msg.shard_index;

        // Verify this is a response to an active request.
        if let Some(req) = self.active_requests.get(&shard_index) {
            if req.peer_id != peer_id {
                tracing::warn!(
                    "received shard data for index {} from unexpected peer {} (expected {})",
                    shard_index,
                    peer_id,
                    req.peer_id
                );
                return 0;
            }
        } else {
            tracing::debug!(
                "received unsolicited shard data for index {} from {}",
                shard_index,
                peer_id
            );
            return 0;
        }

        let entry_count = msg.ledgers.len();
        let mut imported = 0u32;

        {
            let mut manager = self.shard_manager.write().await;
            for entry in msg.ledgers {
                let hash = Hash256::new(entry.hash);
                manager.import_ledger(entry.seq, hash, entry.data);
                imported += 1;
            }
        }

        tracing::info!(
            "imported {}/{} ledger entries for shard {} from peer {}",
            imported,
            entry_count,
            shard_index,
            peer_id
        );

        // Check if shard is now complete.
        let is_complete = {
            let manager = self.shard_manager.read().await;
            manager.is_complete(shard_index)
        };

        if is_complete {
            tracing::info!("shard {} download complete", shard_index);
            self.active_requests.remove(&shard_index);
        }

        imported
    }

    /// Select the best peer for a shard and build a request.
    ///
    /// Returns `None` if no peer has the requested shard or we are at capacity.
    pub async fn request_shard(&mut self, shard_index: u32) -> Option<(Hash256, TMGetShardData)> {
        if self.active_requests.len() >= self.max_concurrent {
            return None;
        }

        if self.active_requests.contains_key(&shard_index) {
            return None;
        }

        // Find a peer that has this shard.
        let peer_id = self
            .peer_shards
            .iter()
            .find(|(_, indices)| indices.contains(&shard_index))
            .map(|(id, _)| *id)?;

        // Determine which sequences we still need.
        let ledger_seqs = {
            let manager = self.shard_manager.read().await;
            let (first, last) = rxrpl_nodestore::shard_range(shard_index);
            let mut needed = Vec::new();
            for seq in first..=last {
                if manager.get_ledger_data(shard_index, seq).is_none() {
                    needed.push(seq);
                    if needed.len() >= MAX_SEQS_PER_REQUEST {
                        break;
                    }
                }
            }
            needed
        };

        if ledger_seqs.is_empty() {
            return None;
        }

        self.active_requests.insert(
            shard_index,
            ActiveRequest {
                shard_index,
                peer_id,
                sent_at: Instant::now(),
                retries: 0,
            },
        );

        Some((
            peer_id,
            TMGetShardData {
                shard_index,
                ledger_seqs,
            },
        ))
    }

    /// Periodic tick: check timeouts, process download queue, request missing data.
    ///
    /// Returns a list of `(peer_id, request)` pairs to send.
    pub async fn tick(&mut self) -> Vec<(Hash256, TMGetShardData)> {
        let now = Instant::now();
        let mut requests = Vec::new();

        // Check for timed-out requests.
        let timed_out: Vec<u32> = self
            .active_requests
            .iter()
            .filter(|(_, req)| now.duration_since(req.sent_at) > REQUEST_TIMEOUT)
            .map(|(idx, _)| *idx)
            .collect();

        for shard_index in timed_out {
            if let Some(mut req) = self.active_requests.remove(&shard_index) {
                req.retries += 1;
                if req.retries > MAX_RETRIES {
                    tracing::warn!(
                        "giving up on shard {} after {} retries",
                        shard_index,
                        req.retries
                    );
                    // Re-queue for later if desired.
                    if !self.download_queue.contains(&shard_index) {
                        self.download_queue.push(shard_index);
                    }
                } else {
                    tracing::debug!(
                        "shard {} request timed out (retry {}), re-requesting",
                        shard_index,
                        req.retries
                    );
                    // Try re-requesting, possibly from a different peer.
                    if let Some((peer_id, request)) = self.request_shard(shard_index).await {
                        // Update retry count.
                        if let Some(active) = self.active_requests.get_mut(&shard_index) {
                            active.retries = req.retries;
                        }
                        requests.push((peer_id, request));
                    }
                }
            }
        }

        // Process queued downloads.
        while self.active_requests.len() < self.max_concurrent {
            if let Some(shard_index) = self.download_queue.pop() {
                if let Some((peer_id, request)) = self.request_shard(shard_index).await {
                    requests.push((peer_id, request));
                }
            } else {
                break;
            }
        }

        requests
    }

    /// Clean up state when a peer disconnects.
    pub fn peer_disconnected(&mut self, peer_id: &Hash256) {
        self.peer_shards.remove(peer_id);

        // Re-queue any active requests that were going to this peer.
        let affected: Vec<u32> = self
            .active_requests
            .iter()
            .filter(|(_, req)| req.peer_id == *peer_id)
            .map(|(idx, _)| *idx)
            .collect();

        for shard_index in affected {
            self.active_requests.remove(&shard_index);
            if !self.download_queue.contains(&shard_index) {
                self.download_queue.push(shard_index);
            }
            tracing::debug!(
                "re-queued shard {} download after peer {} disconnected",
                shard_index,
                peer_id
            );
        }
    }

    /// Queue a shard index for download.
    pub fn queue_download(&mut self, shard_index: u32) {
        if !self.download_queue.contains(&shard_index)
            && !self.active_requests.contains_key(&shard_index)
        {
            self.download_queue.push(shard_index);
        }
    }

    /// Return the set of peers and their known shard indices.
    pub fn peer_shard_info(&self) -> &HashMap<Hash256, Vec<u32>> {
        &self.peer_shards
    }

    /// Number of active download requests.
    pub fn active_count(&self) -> usize {
        self.active_requests.len()
    }

    /// Number of shards queued for download.
    pub fn queued_count(&self) -> usize {
        self.download_queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_syncer() -> ShardSyncer {
        let manager = Arc::new(RwLock::new(ShardManager::new()));
        ShardSyncer::new(manager)
    }

    fn peer_id(val: u8) -> Hash256 {
        Hash256::new([val; 32])
    }

    #[test]
    fn on_shards_message_tracks_peer() {
        let mut syncer = make_syncer();
        let msg = TMShards {
            indices: vec![0, 1, 5],
            incomplete: vec![],
        };
        syncer.on_shards_message(peer_id(1), msg);

        assert_eq!(syncer.peer_shards.get(&peer_id(1)).unwrap(), &[0, 1, 5]);
    }

    #[test]
    fn peer_disconnected_requeues() {
        let mut syncer = make_syncer();
        let pid = peer_id(1);

        // Simulate an active request from this peer.
        syncer.active_requests.insert(
            3,
            ActiveRequest {
                shard_index: 3,
                peer_id: pid,
                sent_at: Instant::now(),
                retries: 0,
            },
        );
        syncer.peer_shards.insert(pid, vec![3]);

        syncer.peer_disconnected(&pid);

        assert!(syncer.active_requests.is_empty());
        assert!(syncer.download_queue.contains(&3));
        assert!(!syncer.peer_shards.contains_key(&pid));
    }

    #[test]
    fn queue_download_prevents_duplicates() {
        let mut syncer = make_syncer();
        syncer.queue_download(5);
        syncer.queue_download(5);
        assert_eq!(syncer.queued_count(), 1);
    }

    #[tokio::test]
    async fn request_shard_requires_peer() {
        let mut syncer = make_syncer();
        // No peers registered.
        assert!(syncer.request_shard(0).await.is_none());
    }

    #[tokio::test]
    async fn request_shard_picks_peer_with_shard() {
        let mut syncer = make_syncer();
        let pid = peer_id(1);
        syncer.peer_shards.insert(pid, vec![0, 1, 2]);

        let result = syncer.request_shard(0).await;
        assert!(result.is_some());
        let (chosen_peer, request) = result.unwrap();
        assert_eq!(chosen_peer, pid);
        assert_eq!(request.shard_index, 0);
        assert!(!request.ledger_seqs.is_empty());
        assert_eq!(syncer.active_count(), 1);
    }

    #[tokio::test]
    async fn request_shard_respects_max_concurrent() {
        let mut syncer = make_syncer();
        let pid = peer_id(1);
        syncer.peer_shards.insert(pid, vec![0, 1, 2, 3, 4]);

        // Fill up to max_concurrent.
        for i in 0..DEFAULT_MAX_CONCURRENT as u32 {
            let _ = syncer.request_shard(i).await;
        }
        assert_eq!(syncer.active_count(), DEFAULT_MAX_CONCURRENT);

        // Next request should return None.
        let result = syncer.request_shard(DEFAULT_MAX_CONCURRENT as u32).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn on_shard_data_imports_entries() {
        let manager = Arc::new(RwLock::new(ShardManager::new()));
        let mut syncer = ShardSyncer::new(Arc::clone(&manager));
        let pid = peer_id(1);

        // Set up an active request.
        syncer.active_requests.insert(
            0,
            ActiveRequest {
                shard_index: 0,
                peer_id: pid,
                sent_at: Instant::now(),
                retries: 0,
            },
        );

        let msg = TMShardData {
            shard_index: 0,
            ledgers: vec![
                rxrpl_p2p_proto::shard_msg::ShardLedgerEntry {
                    seq: 0,
                    hash: [0xAA; 32],
                    data: vec![1, 2, 3],
                },
                rxrpl_p2p_proto::shard_msg::ShardLedgerEntry {
                    seq: 1,
                    hash: [0xBB; 32],
                    data: vec![4, 5, 6],
                },
            ],
        };

        let imported = syncer.on_shard_data(pid, msg).await;
        assert_eq!(imported, 2);

        let mgr = manager.read().await;
        let info = mgr.shard_info(0).unwrap();
        assert_eq!(
            info.state,
            rxrpl_nodestore::ShardState::Incomplete { stored_count: 2 }
        );
    }

    #[tokio::test]
    async fn on_shard_data_rejects_wrong_peer() {
        let manager = Arc::new(RwLock::new(ShardManager::new()));
        let mut syncer = ShardSyncer::new(Arc::clone(&manager));
        let pid1 = peer_id(1);
        let pid2 = peer_id(2);

        syncer.active_requests.insert(
            0,
            ActiveRequest {
                shard_index: 0,
                peer_id: pid1,
                sent_at: Instant::now(),
                retries: 0,
            },
        );

        let msg = TMShardData {
            shard_index: 0,
            ledgers: vec![rxrpl_p2p_proto::shard_msg::ShardLedgerEntry {
                seq: 0,
                hash: [0xAA; 32],
                data: vec![1, 2, 3],
            }],
        };

        // Wrong peer.
        let imported = syncer.on_shard_data(pid2, msg).await;
        assert_eq!(imported, 0);
    }
}
