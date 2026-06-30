use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dashmap::DashMap;
use rxrpl_primitives::Hash256;

use crate::peer_score::PeerScore;
use crate::rate_limiter::PeerRateLimiter;
use crate::reputation::PeerReputation;

/// Thread-safe collection of connected peers.
///
/// Beyond the total `max_peers` cap, the set enforces two rippled-style
/// eclipse-attack defenses: a reserved block of outbound-only slots
/// (`max_inbound = max_peers - reserved_outbound_slots`) so inbound peers can
/// never fill every slot, and a per-remote-IP cap (`max_peers_per_ip`).
pub struct PeerSet {
    peers: DashMap<Hash256, Arc<PeerInfo>>,
    /// Live count of peers per remote IP (key = IP portion of `address`).
    ip_counts: DashMap<String, usize>,
    max_peers: usize,
    /// Maximum inbound peers; the remaining slots are reserved for outbound.
    max_inbound: usize,
    /// Maximum simultaneous peers sharing one remote IP.
    max_peers_per_ip: usize,
    /// Live count of inbound peers (a subset of `peers`).
    inbound_count: AtomicUsize,
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
    /// The peer's 33-byte node public key, advertised in the crawl response.
    pub public_key: Vec<u8>,
    /// When the connection was established, used for crawl `uptime`.
    pub connected_at: std::time::Instant,
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
    /// Build a peer set.
    ///
    /// `reserved_outbound_slots` slots are withheld from inbound peers so the
    /// node always has room for the outbound connections it dials itself;
    /// `max_peers_per_ip` bounds how many peers a single remote IP may hold.
    pub fn new(max_peers: usize, reserved_outbound_slots: usize, max_peers_per_ip: usize) -> Self {
        Self {
            peers: DashMap::new(),
            ip_counts: DashMap::new(),
            max_peers,
            max_inbound: max_peers.saturating_sub(reserved_outbound_slots),
            max_peers_per_ip,
            inbound_count: AtomicUsize::new(0),
        }
    }

    /// Extract the IP portion of an `IP:port` address.
    ///
    /// Splits on the LAST `:` so bracketed/`host:port` IPv6 forms still yield a
    /// stable key; an address with no `:` is used verbatim.
    fn ip_of(address: &str) -> &str {
        match address.rsplit_once(':') {
            Some((ip, _port)) => ip,
            None => address,
        }
    }

    /// Add a peer. Returns false if any slot limit rejects it:
    /// (a) the total `max_peers` cap, (b) the inbound reservation
    /// (`max_inbound`) for inbound peers, or (c) the per-IP cap.
    /// Counters are only bumped once all checks pass, so a rejected peer is
    /// never counted (keeping `remove` symmetric).
    pub fn add(&self, info: Arc<PeerInfo>) -> bool {
        // (a) Total cap (unchanged). Mirrors the original behavior of
        // returning false when full, even for an already-present node_id.
        if self.peers.len() >= self.max_peers {
            return false;
        }
        // Re-adding an existing node_id is an in-place update: it changes no
        // counter (preserves the original "don't double count" semantics).
        let already_present = self.peers.contains_key(&info.node_id);
        // (b) Inbound reservation: keep the outbound-only slots free.
        if !already_present
            && info.inbound
            && self.inbound_count.load(Ordering::Relaxed) >= self.max_inbound
        {
            return false;
        }
        // (c) Per-IP cap.
        let ip = Self::ip_of(&info.address).to_string();
        if !already_present {
            let cur = self.ip_counts.get(&ip).map(|r| *r.value()).unwrap_or(0);
            if cur >= self.max_peers_per_ip {
                return false;
            }
        }
        let inbound = info.inbound;
        let prev = self.peers.insert(info.node_id, info);
        if prev.is_none() {
            if inbound {
                self.inbound_count.fetch_add(1, Ordering::Relaxed);
            }
            *self.ip_counts.entry(ip).or_insert(0) += 1;
        }
        true
    }

    /// Remove a peer by node ID, decrementing the inbound and per-IP counters
    /// so they stay in lock-step with `add`.
    pub fn remove(&self, node_id: &Hash256) -> Option<Arc<PeerInfo>> {
        let removed = self.peers.remove(node_id).map(|(_, v)| v);
        if let Some(info) = &removed {
            if info.inbound {
                self.inbound_count.fetch_sub(1, Ordering::Relaxed);
            }
            let ip = Self::ip_of(&info.address);
            let now_zero = match self.ip_counts.get_mut(ip) {
                Some(mut e) => {
                    *e = e.saturating_sub(1);
                    *e == 0
                }
                None => false,
            };
            if now_zero {
                // Atomically drop the entry only if a concurrent add did not
                // re-increment it in the meantime.
                self.ip_counts.remove_if(ip, |_, &v| v == 0);
            }
        }
        removed
    }

    /// Current number of inbound peers.
    pub fn inbound_count(&self) -> usize {
        self.inbound_count.load(Ordering::Relaxed)
    }

    /// Number of peers currently connected from the given remote IP
    /// (pass the IP portion only, e.g. `"1.2.3.4"`).
    pub fn peers_for_ip(&self, ip: &str) -> usize {
        self.ip_counts.get(ip).map(|r| *r.value()).unwrap_or(0)
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

        candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
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
        make_peer_at(
            id_byte,
            inbound,
            format!("127.0.0.1:{}", 51235 + id_byte as u16),
        )
    }

    fn make_peer_at(id_byte: u8, inbound: bool, address: String) -> Arc<PeerInfo> {
        Arc::new(PeerInfo {
            node_id: Hash256::new([id_byte; 32]),
            address,
            inbound,
            public_key: vec![0x03; 33],
            connected_at: std::time::Instant::now(),
            ledger_seq: std::sync::atomic::AtomicU32::new(0),
            reputation: PeerReputation::new(),
            scoring: PeerScore::new(),
            rate_limiter: PeerRateLimiter::default(),
            software: PeerSoftware::Unknown,
        })
    }

    /// Old behavior for tests that predate the eclipse limits: no reserved
    /// outbound slots and an effectively unlimited per-IP cap.
    fn unlimited(max_peers: usize) -> PeerSet {
        PeerSet::new(max_peers, 0, usize::MAX)
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
        let set = unlimited(10);
        let peer = make_peer(1, false);
        assert!(set.add(peer.clone()));
        assert_eq!(set.len(), 1);
        assert!(set.get(&Hash256::new([1; 32])).is_some());
    }

    #[test]
    fn peer_limit() {
        let set = unlimited(1);
        assert!(set.add(make_peer(1, false)));
        assert!(!set.add(make_peer(2, false)));
    }

    #[test]
    fn remove_peer() {
        let set = unlimited(10);
        let id = Hash256::new([1; 32]);
        set.add(make_peer(1, false));
        assert!(set.remove(&id).is_some());
        assert!(set.is_empty());
    }

    #[allow(dead_code)]
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
        let set = unlimited(10);
        assert!(set.best_peers(3).is_empty());
    }

    #[test]
    fn best_peers_filters_negative() {
        let set = unlimited(10);
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
        let set = unlimited(10);

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
        let set = unlimited(10);

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

    #[test]
    fn reserved_outbound_slots_block_excess_inbound() {
        // max_peers=10, reserved=6 => max_inbound=4. Per-IP cap high so it does
        // not interfere (all test peers share 127.0.0.1).
        let set = PeerSet::new(10, 6, usize::MAX);

        // 4 inbound peers fill the inbound allowance.
        for i in 1..=4u8 {
            assert!(set.add(make_peer(i, true)), "inbound {i} should fit");
        }
        assert_eq!(set.inbound_count(), 4);

        // The 5th inbound is rejected -- the remaining 6 slots are reserved.
        assert!(
            !set.add(make_peer(5, true)),
            "5th inbound must be rejected by the reservation"
        );
        assert_eq!(set.inbound_count(), 4);

        // An outbound peer still succeeds: the reserved slots are for it.
        assert!(
            set.add(make_peer(6, false)),
            "outbound must still fit in a reserved slot"
        );
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn per_ip_cap_rejects_third_peer_from_same_ip() {
        // No inbound reservation; cap = 2 peers per IP.
        let set = PeerSet::new(10, 0, 2);

        assert!(set.add(make_peer_at(1, false, "1.2.3.4:1000".into())));
        assert!(set.add(make_peer_at(2, false, "1.2.3.4:1001".into())));
        assert_eq!(set.peers_for_ip("1.2.3.4"), 2);

        // Third peer from the same IP (new port, new node_id) is rejected.
        assert!(
            !set.add(make_peer_at(3, false, "1.2.3.4:1002".into())),
            "third peer from same IP must be rejected"
        );
        assert_eq!(set.peers_for_ip("1.2.3.4"), 2);

        // A peer from a different IP still succeeds.
        assert!(set.add(make_peer_at(4, false, "5.6.7.8:1000".into())));
        assert_eq!(set.peers_for_ip("5.6.7.8"), 1);
    }

    #[test]
    fn remove_frees_inbound_and_ip_counters() {
        // max_inbound = 2, per-IP cap = 2.
        let set = PeerSet::new(10, 8, 2);

        let p1 = make_peer_at(1, true, "9.9.9.9:1000".into());
        let p2 = make_peer_at(2, true, "9.9.9.9:1001".into());
        assert!(set.add(Arc::clone(&p1)));
        assert!(set.add(Arc::clone(&p2)));
        assert_eq!(set.inbound_count(), 2);
        assert_eq!(set.peers_for_ip("9.9.9.9"), 2);

        // Both limits are now saturated: a third inbound from the same IP fails
        // on the inbound reservation, and even an outbound from that IP fails
        // on the per-IP cap.
        assert!(!set.add(make_peer_at(3, true, "9.9.9.9:1002".into())));
        assert!(!set.add(make_peer_at(4, false, "9.9.9.9:1003".into())));

        // Removing one peer frees one inbound slot and one IP slot.
        assert!(set.remove(&p1.node_id).is_some());
        assert_eq!(set.inbound_count(), 1);
        assert_eq!(set.peers_for_ip("9.9.9.9"), 1);

        // A fresh peer from the freed IP / inbound slot now succeeds again.
        assert!(set.add(make_peer_at(5, true, "9.9.9.9:1004".into())));
        assert_eq!(set.inbound_count(), 2);
        assert_eq!(set.peers_for_ip("9.9.9.9"), 2);
    }
}
