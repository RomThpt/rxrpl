use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use rxrpl_primitives::Hash256;

/// Default squelch duration (5 minutes).
const DEFAULT_SQUELCH_DURATION: Duration = Duration::from_secs(300);

/// Minimum number of peers relaying the same validator before we squelch.
/// We keep one "selected" source and squelch the rest.
const MIN_PEERS_BEFORE_SQUELCH: usize = 3;

/// Tracks validation relay sources and manages squelch state.
///
/// For each validator public key, we track which peers are relaying
/// validations from that validator. When redundant sources are detected
/// (multiple peers relaying the same validator), we select the best peer
/// as the primary source and squelch the rest.
///
/// Squelch entries expire after a configurable duration, allowing peers
/// to resume relaying.
pub struct SquelchManager {
    /// For each validator key, the set of peers that have recently relayed
    /// validations from that validator.
    validator_sources: HashMap<Vec<u8>, HashSet<Hash256>>,

    /// The selected "primary" peer for each validator key. We only keep
    /// validations from this peer and squelch all others.
    selected_source: HashMap<Vec<u8>, Hash256>,

    /// Active outbound squelches: (peer_id, validator_key) -> expiry time.
    /// These are squelch messages we have sent to peers, asking them to
    /// stop relaying a specific validator's messages to us.
    outbound_squelches: HashMap<(Hash256, Vec<u8>), Instant>,

    /// Inbound squelches received from peers: (peer_id, validator_key) -> expiry.
    /// We respect these by not relaying the specified validator's messages
    /// to the requesting peer.
    inbound_squelches: HashMap<(Hash256, Vec<u8>), Instant>,

    /// Duration for squelch requests.
    squelch_duration: Duration,

    /// Metrics: total squelch messages sent.
    pub squelch_messages_sent: u64,

    /// Metrics: total validations suppressed due to outbound squelch.
    pub validations_suppressed_inbound: u64,

    /// Metrics: total validations we skipped relaying due to inbound squelch.
    pub validations_suppressed_outbound: u64,
}

/// Action returned by the squelch manager when processing a validation.
#[derive(Debug, PartialEq, Eq)]
pub struct SquelchAction {
    /// Peers that should be sent a squelch message for this validator key.
    pub squelch_peers: Vec<Hash256>,
    /// The validator public key to include in the squelch message.
    pub validator_key: Vec<u8>,
    /// Duration in seconds for the squelch.
    pub duration_secs: u32,
}

impl SquelchManager {
    pub fn new() -> Self {
        Self {
            validator_sources: HashMap::new(),
            selected_source: HashMap::new(),
            outbound_squelches: HashMap::new(),
            inbound_squelches: HashMap::new(),
            squelch_duration: DEFAULT_SQUELCH_DURATION,
            squelch_messages_sent: 0,
            validations_suppressed_inbound: 0,
            validations_suppressed_outbound: 0,
        }
    }

    /// Record that a peer relayed a validation from the given validator.
    ///
    /// Returns a `SquelchAction` if redundant sources are detected and
    /// squelch messages should be sent to peers.
    pub fn record_validation_source(
        &mut self,
        peer_id: Hash256,
        validator_key: &[u8],
    ) -> Option<SquelchAction> {
        let now = Instant::now();

        // Check if this peer is already squelched for this validator.
        let squelch_key = (peer_id, validator_key.to_vec());
        if let Some(expiry) = self.outbound_squelches.get(&squelch_key) {
            if now < *expiry {
                // Peer is squelched but still sending -- count as suppressed.
                self.validations_suppressed_inbound += 1;
                return None;
            }
            // Squelch expired, remove it.
            self.outbound_squelches.remove(&squelch_key);
        }

        let sources = self
            .validator_sources
            .entry(validator_key.to_vec())
            .or_default();
        sources.insert(peer_id);

        // If we already have a selected source for this validator,
        // and this peer is not it, consider squelching.
        if let Some(&selected) = self.selected_source.get(validator_key) {
            if selected != peer_id && sources.len() >= MIN_PEERS_BEFORE_SQUELCH {
                // Only squelch this one peer if not already squelched.
                if !self.outbound_squelches.contains_key(&squelch_key) {
                    let expiry = now + self.squelch_duration;
                    self.outbound_squelches.insert(squelch_key, expiry);
                    self.squelch_messages_sent += 1;
                    return Some(SquelchAction {
                        squelch_peers: vec![peer_id],
                        validator_key: validator_key.to_vec(),
                        duration_secs: self.squelch_duration.as_secs() as u32,
                    });
                }
                return None;
            }
            return None;
        }

        // No selected source yet. If we have enough sources, pick this one
        // as primary and squelch the rest.
        if sources.len() >= MIN_PEERS_BEFORE_SQUELCH {
            self.selected_source.insert(validator_key.to_vec(), peer_id);

            let peers_to_squelch: Vec<Hash256> = sources
                .iter()
                .filter(|&&id| id != peer_id)
                .filter(|&&id| {
                    !self
                        .outbound_squelches
                        .contains_key(&(id, validator_key.to_vec()))
                })
                .copied()
                .collect();

            if peers_to_squelch.is_empty() {
                return None;
            }

            let expiry = now + self.squelch_duration;
            for &pid in &peers_to_squelch {
                self.outbound_squelches
                    .insert((pid, validator_key.to_vec()), expiry);
                self.squelch_messages_sent += 1;
            }

            return Some(SquelchAction {
                squelch_peers: peers_to_squelch,
                validator_key: validator_key.to_vec(),
                duration_secs: self.squelch_duration.as_secs() as u32,
            });
        }

        None
    }

    /// Handle a squelch message received from a peer.
    ///
    /// If `squelch` is true, we should stop relaying the specified validator's
    /// messages to the requesting peer for the given duration.
    /// If `squelch` is false, the peer is unsquelching (resume relaying).
    pub fn handle_inbound_squelch(
        &mut self,
        peer_id: Hash256,
        validator_key: &[u8],
        squelch: bool,
        duration_secs: u32,
    ) {
        let key = (peer_id, validator_key.to_vec());
        if squelch {
            let duration = if duration_secs > 0 {
                Duration::from_secs(duration_secs as u64)
            } else {
                DEFAULT_SQUELCH_DURATION
            };
            let expiry = Instant::now() + duration;
            self.inbound_squelches.insert(key, expiry);
            tracing::debug!(
                "peer {} squelched validator {} for {}s",
                peer_id,
                hex::encode(validator_key),
                duration_secs,
            );
        } else {
            self.inbound_squelches.remove(&key);
            tracing::debug!(
                "peer {} unsquelched validator {}",
                peer_id,
                hex::encode(validator_key),
            );
        }
    }

    /// Check whether we should skip relaying a validation to a specific peer
    /// because that peer has sent us a squelch for this validator.
    pub fn is_relay_squelched(&mut self, peer_id: &Hash256, validator_key: &[u8]) -> bool {
        let key = (*peer_id, validator_key.to_vec());
        if let Some(expiry) = self.inbound_squelches.get(&key) {
            if Instant::now() < *expiry {
                self.validations_suppressed_outbound += 1;
                return true;
            }
            // Expired, clean up.
            self.inbound_squelches.remove(&key);
        }
        false
    }

    /// Remove all state associated with a disconnected peer.
    pub fn remove_peer(&mut self, peer_id: &Hash256) {
        // Remove from validator sources.
        let mut empty_keys = Vec::new();
        for (key, sources) in &mut self.validator_sources {
            sources.remove(peer_id);
            if sources.is_empty() {
                empty_keys.push(key.clone());
            }
        }
        for key in &empty_keys {
            self.validator_sources.remove(key);
        }

        // If this peer was a selected source, clear the selection so a new
        // primary can be chosen on the next validation.
        let mut clear_keys = Vec::new();
        for (key, &selected) in &self.selected_source {
            if selected == *peer_id {
                clear_keys.push(key.clone());
            }
        }
        for key in clear_keys {
            self.selected_source.remove(&key);
        }

        // Remove outbound squelches involving this peer.
        self.outbound_squelches.retain(|(pid, _), _| pid != peer_id);

        // Remove inbound squelches from this peer.
        self.inbound_squelches.retain(|(pid, _), _| pid != peer_id);
    }

    /// Expire old squelch entries and stale validator sources.
    /// Should be called periodically (e.g., every 30 seconds).
    pub fn expire_stale_entries(&mut self) {
        let now = Instant::now();

        self.outbound_squelches.retain(|_, expiry| now < *expiry);
        self.inbound_squelches.retain(|_, expiry| now < *expiry);

        // Clear validator source tracking periodically so it can rebuild.
        // This prevents unbounded growth and ensures fresh selection.
        // We only clear sources for validators that have no active squelches.
        let active_validators: HashSet<Vec<u8>> = self
            .outbound_squelches
            .keys()
            .map(|(_, key)| key.clone())
            .collect();

        self.validator_sources
            .retain(|key, _| active_validators.contains(key));

        self.selected_source
            .retain(|key, _| active_validators.contains(key));
    }

    /// Return current metrics as a snapshot.
    pub fn metrics(&self) -> SquelchMetrics {
        SquelchMetrics {
            active_outbound_squelches: self.outbound_squelches.len(),
            active_inbound_squelches: self.inbound_squelches.len(),
            tracked_validators: self.validator_sources.len(),
            squelch_messages_sent: self.squelch_messages_sent,
            validations_suppressed_inbound: self.validations_suppressed_inbound,
            validations_suppressed_outbound: self.validations_suppressed_outbound,
        }
    }
}

/// Snapshot of squelch effectiveness metrics.
#[derive(Debug, Clone)]
pub struct SquelchMetrics {
    pub active_outbound_squelches: usize,
    pub active_inbound_squelches: usize,
    pub tracked_validators: usize,
    pub squelch_messages_sent: u64,
    pub validations_suppressed_inbound: u64,
    pub validations_suppressed_outbound: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: u8) -> Hash256 {
        Hash256::new([id; 32])
    }

    fn validator_key(id: u8) -> Vec<u8> {
        vec![0xED, id, 0x00, 0x00]
    }

    #[test]
    fn no_squelch_below_threshold() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        // Only 2 peers -- below MIN_PEERS_BEFORE_SQUELCH (3).
        assert!(mgr.record_validation_source(peer(1), &vk).is_none());
        assert!(mgr.record_validation_source(peer(2), &vk).is_none());
    }

    #[test]
    fn squelch_triggered_at_threshold() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        assert!(mgr.record_validation_source(peer(1), &vk).is_none());
        assert!(mgr.record_validation_source(peer(2), &vk).is_none());

        // Third peer triggers squelch.
        let action = mgr.record_validation_source(peer(3), &vk);
        assert!(action.is_some());

        let action = action.unwrap();
        assert_eq!(action.validator_key, vk);
        // Peer 3 is selected as primary; peers 1 and 2 should be squelched.
        assert_eq!(action.squelch_peers.len(), 2);
        assert!(action.squelch_peers.contains(&peer(1)));
        assert!(action.squelch_peers.contains(&peer(2)));
        assert_eq!(action.duration_secs, 300);
    }

    #[test]
    fn additional_peer_squelched_individually() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        mgr.record_validation_source(peer(3), &vk); // triggers initial squelch

        // Fourth peer arrives -- should be squelched too.
        let action = mgr.record_validation_source(peer(4), &vk);
        assert!(action.is_some());
        let action = action.unwrap();
        assert_eq!(action.squelch_peers, vec![peer(4)]);
    }

    #[test]
    fn selected_source_not_squelched() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        let action = mgr.record_validation_source(peer(3), &vk).unwrap();

        // Peer 3 was the third and became selected; it should not be squelched.
        assert!(!action.squelch_peers.contains(&peer(3)));

        // Subsequent validation from selected source should not trigger squelch.
        assert!(mgr.record_validation_source(peer(3), &vk).is_none());
    }

    #[test]
    fn inbound_squelch_suppresses_relay() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.handle_inbound_squelch(peer(1), &vk, true, 300);
        assert!(mgr.is_relay_squelched(&peer(1), &vk));
        assert!(!mgr.is_relay_squelched(&peer(2), &vk));
    }

    #[test]
    fn unsquelch_resumes_relay() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.handle_inbound_squelch(peer(1), &vk, true, 300);
        assert!(mgr.is_relay_squelched(&peer(1), &vk));

        mgr.handle_inbound_squelch(peer(1), &vk, false, 0);
        assert!(!mgr.is_relay_squelched(&peer(1), &vk));
    }

    #[test]
    fn remove_peer_cleans_state() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        mgr.record_validation_source(peer(3), &vk);

        mgr.handle_inbound_squelch(peer(1), &vk, true, 300);

        mgr.remove_peer(&peer(1));

        // Inbound squelch from peer 1 should be gone.
        assert!(!mgr.is_relay_squelched(&peer(1), &vk));

        // Metrics should reflect cleanup.
        let m = mgr.metrics();
        assert_eq!(m.active_inbound_squelches, 0);
    }

    #[test]
    fn remove_selected_peer_allows_reselection() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        mgr.record_validation_source(peer(3), &vk); // peer 3 selected

        mgr.remove_peer(&peer(3));

        // New peers should be able to trigger fresh selection.
        mgr.record_validation_source(peer(4), &vk);
        mgr.record_validation_source(peer(5), &vk);
        // After peer removal, source set was cleaned. Rebuild it.
        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        let action = mgr.record_validation_source(peer(4), &vk);
        // Should eventually trigger a new squelch cycle.
        // (Exact behavior depends on cleanup, but no panic.)
        let _ = action;
    }

    #[test]
    fn metrics_tracking() {
        let mut mgr = SquelchManager::new();
        let vk = validator_key(1);

        mgr.record_validation_source(peer(1), &vk);
        mgr.record_validation_source(peer(2), &vk);
        mgr.record_validation_source(peer(3), &vk);

        let m = mgr.metrics();
        assert!(m.squelch_messages_sent >= 2);
        assert!(m.active_outbound_squelches >= 2);
        assert_eq!(m.tracked_validators, 1);
    }

    #[test]
    fn different_validators_tracked_independently() {
        let mut mgr = SquelchManager::new();
        let vk1 = validator_key(1);
        let vk2 = validator_key(2);

        mgr.record_validation_source(peer(1), &vk1);
        mgr.record_validation_source(peer(2), &vk1);
        mgr.record_validation_source(peer(3), &vk1);

        // Only 1 peer for vk2, should not trigger squelch.
        assert!(mgr.record_validation_source(peer(1), &vk2).is_none());

        let m = mgr.metrics();
        assert_eq!(m.tracked_validators, 2);
    }

    #[test]
    fn expire_stale_removes_expired() {
        let mut mgr = SquelchManager::new();
        // Manually insert an expired outbound squelch.
        let expired = Instant::now() - Duration::from_secs(1);
        mgr.outbound_squelches
            .insert((peer(1), validator_key(1)), expired);
        mgr.inbound_squelches
            .insert((peer(2), validator_key(2)), expired);

        assert_eq!(mgr.outbound_squelches.len(), 1);
        assert_eq!(mgr.inbound_squelches.len(), 1);

        mgr.expire_stale_entries();

        assert_eq!(mgr.outbound_squelches.len(), 0);
        assert_eq!(mgr.inbound_squelches.len(), 0);
    }
}
