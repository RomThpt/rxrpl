use std::sync::Arc;

use dashmap::DashMap;
use rxrpl_primitives::Hash256;

use crate::peer_score::PeerScore;
use crate::rate_limiter::PeerRateLimiter;
use crate::reputation::PeerReputation;

/// Thread-safe collection of connected peers.
pub struct PeerSet {
    peers: DashMap<Hash256, Arc<PeerInfo>>,
    max_peers: usize,
}

/// Software identity of a remote peer (parsed from `User-Agent` header).
///
/// Used for telemetry and (in the future) protocol-version-specific behavior.
/// The wire layer is unified on rippled format and does not branch on this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerSoftware {
    /// Another rxrpl node. Carries the version string.
    Rxrpl(String),
    /// A rippled node. Carries the version string.
    Rippled(String),
    /// Anything else. Carries the raw `User-Agent` value.
    Other(String),
    /// Peer did not advertise a `User-Agent`.
    Unknown,
}

impl PeerSoftware {
    /// Parse a raw `User-Agent` header value (e.g. `rippled-2.5.0` or `rxrpl/0.1.0`).
    pub fn parse(ua: &str) -> Self {
        let trimmed = ua.trim();
        if trimmed.is_empty() {
            return PeerSoftware::Unknown;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(rest) = lower
            .strip_prefix("rxrpl/")
            .or_else(|| lower.strip_prefix("rxrpl-"))
        {
            PeerSoftware::Rxrpl(rest.to_string())
        } else if let Some(rest) = lower
            .strip_prefix("rippled-")
            .or_else(|| lower.strip_prefix("rippled/"))
        {
            PeerSoftware::Rippled(rest.to_string())
        } else {
            PeerSoftware::Other(trimmed.to_string())
        }
    }
}

/// Information about a connected peer.
#[derive(Debug)]
pub struct PeerInfo {
    /// The peer's node ID.
    pub node_id: Hash256,
    /// Remote address.
    pub address: String,
    /// Whether this is an inbound or outbound connection.
    pub inbound: bool,
    /// Last known ledger sequence from this peer.
    pub ledger_seq: std::sync::atomic::AtomicU32,
    /// Reputation score tracking.
    pub reputation: PeerReputation,
    /// Scoring metrics for peer selection.
    pub scoring: PeerScore,
    /// Per-peer message rate limiter.
    pub rate_limiter: PeerRateLimiter,
    /// Peer's software identity from the handshake `User-Agent` header.
    pub software: PeerSoftware,
}

impl PeerSet {
    pub fn new(max_peers: usize) -> Self {
        Self {
            peers: DashMap::new(),
            max_peers,
        }
    }

    /// Add a peer. Returns false if the peer limit is reached.
    pub fn add(&self, info: Arc<PeerInfo>) -> bool {
        if self.peers.len() >= self.max_peers {
            return false;
        }
        self.peers.insert(info.node_id, info);
        true
    }

    /// Remove a peer by node ID.
    pub fn remove(&self, node_id: &Hash256) -> Option<Arc<PeerInfo>> {
        self.peers.remove(node_id).map(|(_, v)| v)
    }

    /// Get a peer by node ID.
    pub fn get(&self, node_id: &Hash256) -> Option<Arc<PeerInfo>> {
        self.peers.get(node_id).map(|r| Arc::clone(r.value()))
    }

    /// Number of connected peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Get all peer node IDs.
    pub fn peer_ids(&self) -> Vec<Hash256> {
        self.peers.iter().map(|r| *r.key()).collect()
    }

    /// Get all peer infos for iteration.
    pub fn all_peers(&self) -> Vec<Arc<PeerInfo>> {
        self.peers.iter().map(|r| Arc::clone(r.value())).collect()
    }

    /// Select up to `count` peers sorted by reputation score (highest first).
    /// Only includes peers with non-negative scores.
    /// Ties are broken by latency ascending (lower latency preferred).
    pub fn best_peers(&self, count: usize) -> Vec<Hash256> {
        let mut candidates: Vec<_> = self
            .peers
            .iter()
            .filter(|r| r.value().reputation.score() >= 0)
            .map(|r| {
                let info = r.value();
                let score = info.reputation.score();
                let latency = info.reputation.avg_latency_ms().unwrap_or(u64::MAX);
                (info.node_id, score, latency)
            })
            .collect();

        // Sort by score descending, then latency ascending for ties
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
        candidates
            .into_iter()
            .take(count)
            .map(|(id, _, _)| id)
            .collect()
    }

    /// Select up to `count` peers sorted by normalized score (0-100, highest first).
    /// Uses the multi-metric scoring algorithm that considers latency, uptime,
    /// message validity, response rate, and base reputation.
    /// Only includes peers with non-negative reputation scores.
    pub fn select_by_normalized_score(&self, count: usize) -> Vec<Hash256> {
        let mut candidates: Vec<_> = self
            .peers
            .iter()
            .filter(|r| r.value().reputation.score() >= 0)
            .map(|r| {
                let info = r.value();
                let norm_score = info.scoring.normalized_score(&info.reputation);
                (info.node_id, norm_score)
            })
            .collect();

        candidates.sort_by(|a, b| b.1.cmp(&a.1));
        candidates
            .into_iter()
            .take(count)
            .map(|(id, _)| id)
            .collect()
    }

    /// Apply temporal decay to all peer scores.
    pub fn apply_score_decay(&self) {
        for entry in self.peers.iter() {
            let info = entry.value();
            info.scoring.apply_decay(&info.reputation);
        }
    }

    /// Select the best peers for ledger data based on score and ledger proximity.
    /// Prefers peers whose known `ledger_seq >= target_seq` by adding a +200 bonus.
    ///
    /// Tries the non-negative-reputation peers first. If the strict filter
    /// excludes everyone (e.g. our only peer has accumulated a small
    /// negative score from an unfamiliar message type), we fall back to
    /// the full peer set so catch-up can still make progress. A node with
    /// no candidate peer at all cannot sync.
    pub fn best_peers_for_ledger(&self, target_seq: u32, count: usize) -> Vec<Hash256> {
        const LEDGER_AHEAD_BONUS: i32 = 200;

        fn rank<'a, I>(it: I, target_seq: u32) -> Vec<(Hash256, i32, u64)>
        where
            I: Iterator<
                Item = dashmap::mapref::multiple::RefMulti<'a, Hash256, std::sync::Arc<PeerInfo>>,
            >,
        {
            let mut candidates: Vec<_> = it
                .map(|r| {
                    let info = r.value();
                    let base_score = info.reputation.score();
                    let peer_seq = info.ledger_seq.load(std::sync::atomic::Ordering::Relaxed);
                    let effective_score = if peer_seq >= target_seq {
                        base_score + LEDGER_AHEAD_BONUS
                    } else {
                        base_score
                    };
                    let latency = info.reputation.avg_latency_ms().unwrap_or(u64::MAX);
                    (info.node_id, effective_score, latency)
                })
                .collect();
            candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.2.cmp(&b.2)));
            candidates
        }

        let strict = rank(
            self.peers
                .iter()
                .filter(|r| r.value().reputation.score() >= 0),
            target_seq,
        );
        if !strict.is_empty() {
            return strict
                .into_iter()
                .take(count)
                .map(|(id, _, _)| id)
                .collect();
        }
        // Fallback: include negative-rep peers rather than return nothing.
        rank(self.peers.iter(), target_seq)
            .into_iter()
            .take(count)
            .map(|(id, _, _)| id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(id_byte: u8, inbound: bool) -> Arc<PeerInfo> {
        Arc::new(PeerInfo {
            node_id: Hash256::new([id_byte; 32]),
            address: format!("127.0.0.1:{}", 51235 + id_byte as u16),
            inbound,
            ledger_seq: std::sync::atomic::AtomicU32::new(0),
            reputation: PeerReputation::new(),
            scoring: PeerScore::new(),
            rate_limiter: PeerRateLimiter::default(),
            software: PeerSoftware::Unknown,
        })
    }

    #[test]
    fn parse_user_agent() {
        assert_eq!(
            PeerSoftware::parse("rippled-2.5.0"),
            PeerSoftware::Rippled("2.5.0".into())
        );
        assert_eq!(
            PeerSoftware::parse("rxrpl/0.1.0"),
            PeerSoftware::Rxrpl("0.1.0".into())
        );
        assert_eq!(
            PeerSoftware::parse("totally-different-impl/1"),
            PeerSoftware::Other("totally-different-impl/1".into())
        );
        assert_eq!(PeerSoftware::parse(""), PeerSoftware::Unknown);
    }

    #[test]
    fn add_and_get() {
        let set = PeerSet::new(10);
        let peer = make_peer(1, false);
        assert!(set.add(peer.clone()));
        assert_eq!(set.len(), 1);
        assert!(set.get(&Hash256::new([1; 32])).is_some());
    }

    #[test]
    fn peer_limit() {
        let set = PeerSet::new(1);
        assert!(set.add(make_peer(1, false)));
        assert!(!set.add(make_peer(2, false)));
    }

    #[test]
    fn remove_peer() {
        let set = PeerSet::new(10);
        let id = Hash256::new([1; 32]);
        set.add(make_peer(1, false));
        assert!(set.remove(&id).is_some());
        assert!(set.is_empty());
    }

    fn make_peer_with_score(id_byte: u8, score_delta: i32) -> Arc<PeerInfo> {
        let peer = make_peer(id_byte, false);
        if score_delta > 0 {
            for _ in 0..score_delta {
                peer.reputation.record_valid_message(1);
            }
        } else if score_delta < 0 {
            // Each invalid message is -10, so use violations (-50) and invalid (-10)
            // For simplicity, adjust via valid/invalid to reach approximate score
            for _ in 0..(-score_delta) {
                peer.reputation.record_invalid_message(); // -10 each
            }
        }
        peer
    }

    #[test]
    fn best_peers_empty() {
        let set = PeerSet::new(10);
        assert!(set.best_peers(3).is_empty());
    }

    #[test]
    fn best_peers_filters_negative() {
        let set = PeerSet::new(10);
        // Peer 1: score 0 (included)
        set.add(make_peer(1, false));
        // Peer 2: negative score (excluded) -- 1 invalid message = -10
        let negative_peer = make_peer(2, false);
        negative_peer.reputation.record_invalid_message();
        set.add(negative_peer);

        let best = set.best_peers(10);
        assert_eq!(best.len(), 1);
        assert_eq!(best[0], Hash256::new([1; 32]));
    }

    #[test]
    fn best_peers_sorted_by_score() {
        let set = PeerSet::new(10);

        // Peer 1: score +5 (5 valid messages)
        let p1 = make_peer(1, false);
        for _ in 0..5 {
            p1.reputation.record_valid_message(1);
        }
        set.add(p1);

        // Peer 2: score +20 (20 valid messages)
        let p2 = make_peer(2, false);
        for _ in 0..20 {
            p2.reputation.record_valid_message(1);
        }
        set.add(p2);

        // Peer 3: score +10 (10 valid messages)
        let p3 = make_peer(3, false);
        for _ in 0..10 {
            p3.reputation.record_valid_message(1);
        }
        set.add(p3);

        let best = set.best_peers(3);
        assert_eq!(best.len(), 3);
        // Highest score first
        assert_eq!(best[0], Hash256::new([2; 32])); // score 20
        assert_eq!(best[1], Hash256::new([3; 32])); // score 10
        assert_eq!(best[2], Hash256::new([1; 32])); // score 5
    }

    #[test]
    fn best_peers_for_ledger_prefers_ahead() {
        let set = PeerSet::new(10);

        // Peer 1: score +10, ledger_seq = 50 (behind target)
        let p1 = make_peer(1, false);
        for _ in 0..10 {
            p1.reputation.record_valid_message(1);
        }
        p1.ledger_seq
            .store(50, std::sync::atomic::Ordering::Relaxed);
        set.add(p1);

        // Peer 2: score +5, ledger_seq = 100 (at target -- gets +200 bonus)
        let p2 = make_peer(2, false);
        for _ in 0..5 {
            p2.reputation.record_valid_message(1);
        }
        p2.ledger_seq
            .store(100, std::sync::atomic::Ordering::Relaxed);
        set.add(p2);

        let best = set.best_peers_for_ledger(100, 2);
        assert_eq!(best.len(), 2);
        // Peer 2 has effective score 5 + 200 = 205, peer 1 has 10
        assert_eq!(best[0], Hash256::new([2; 32]));
        assert_eq!(best[1], Hash256::new([1; 32]));
    }
}
