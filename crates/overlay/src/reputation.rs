use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Minimum reputation score.
const SCORE_MIN: i32 = -1000;
/// Maximum reputation score.
const SCORE_MAX: i32 = 1000;
/// Threshold below which a peer should be disconnected.
const DISCONNECT_THRESHOLD: i32 = -500;

/// Score adjustments for various peer behaviors.
const VALID_MESSAGE_REWARD: i32 = 1;
const INVALID_MESSAGE_PENALTY: i32 = -10;
const USEFUL_CONTRIBUTION_REWARD: i32 = 5;
const VIOLATION_PENALTY: i32 = -50;

/// Peer reputation score tracking.
///
/// Higher score = better peer. Starts at 0.
/// Score range: -1000 to +1000.
pub struct PeerReputation {
    score: AtomicI32,
    messages_received: AtomicU64,
    messages_invalid: AtomicU64,
    bytes_received: AtomicU64,
    latency_sum_ms: AtomicU64,
    pings_completed: AtomicU64,
    connected_at: Instant,
}

impl std::fmt::Debug for PeerReputation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerReputation")
            .field("score", &self.score.load(Ordering::Relaxed))
            .field(
                "messages_received",
                &self.messages_received.load(Ordering::Relaxed),
            )
            .field(
                "messages_invalid",
                &self.messages_invalid.load(Ordering::Relaxed),
            )
            .finish()
    }
}

impl PeerReputation {
    pub fn new() -> Self {
        Self {
            score: AtomicI32::new(0),
            messages_received: AtomicU64::new(0),
            messages_invalid: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            latency_sum_ms: AtomicU64::new(0),
            pings_completed: AtomicU64::new(0),
            connected_at: Instant::now(),
        }
    }

    /// Adjust the score by `delta`, clamping to [SCORE_MIN, SCORE_MAX].
    fn adjust_score(&self, delta: i32) {
        self.score
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(delta).clamp(SCORE_MIN, SCORE_MAX))
            })
            .ok();
    }

    /// Record a valid message received.
    pub fn record_valid_message(&self, bytes: u64) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
        self.adjust_score(VALID_MESSAGE_REWARD);
    }

    /// Record an invalid/malformed message.
    pub fn record_invalid_message(&self) {
        self.messages_invalid.fetch_add(1, Ordering::Relaxed);
        self.adjust_score(INVALID_MESSAGE_PENALTY);
    }

    /// Record a completed ping roundtrip.
    pub fn record_ping_latency(&self, latency_ms: u64) {
        self.latency_sum_ms.fetch_add(latency_ms, Ordering::Relaxed);
        self.pings_completed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a useful contribution (e.g., provided requested ledger data).
    pub fn record_useful_contribution(&self) {
        self.adjust_score(USEFUL_CONTRIBUTION_REWARD);
    }

    /// Record a protocol violation.
    pub fn record_violation(&self) {
        self.adjust_score(VIOLATION_PENALTY);
    }

    /// Get the current reputation score.
    pub fn score(&self) -> i32 {
        self.score.load(Ordering::Relaxed)
    }

    /// Get total messages received.
    pub fn messages_received(&self) -> u64 {
        self.messages_received.load(Ordering::Relaxed)
    }

    /// Get total invalid messages received.
    pub fn messages_invalid(&self) -> u64 {
        self.messages_invalid.load(Ordering::Relaxed)
    }

    /// Get total bytes received.
    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }

    /// Get average latency in ms.
    pub fn avg_latency_ms(&self) -> Option<u64> {
        let pings = self.pings_completed.load(Ordering::Relaxed);
        if pings == 0 {
            return None;
        }
        Some(self.latency_sum_ms.load(Ordering::Relaxed) / pings)
    }

    /// Get uptime since connection.
    pub fn uptime(&self) -> Duration {
        self.connected_at.elapsed()
    }

    /// Check if the peer should be disconnected based on score.
    pub fn should_disconnect(&self) -> bool {
        self.score.load(Ordering::Relaxed) < DISCONNECT_THRESHOLD
    }

    /// Get a JSON summary for the `peers` RPC handler.
    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "score": self.score(),
            "messages_received": self.messages_received(),
            "messages_invalid": self.messages_invalid(),
            "bytes_received": self.bytes_received(),
            "avg_latency_ms": self.avg_latency_ms(),
            "uptime_secs": self.uptime().as_secs(),
        })
    }
}

impl Default for PeerReputation {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_reputation_starts_at_zero() {
        let rep = PeerReputation::new();
        assert_eq!(rep.score(), 0);
        assert_eq!(rep.messages_received(), 0);
        assert_eq!(rep.messages_invalid(), 0);
        assert_eq!(rep.bytes_received(), 0);
        assert_eq!(rep.avg_latency_ms(), None);
        assert!(!rep.should_disconnect());
    }

    #[test]
    fn valid_message_increases_score() {
        let rep = PeerReputation::new();
        rep.record_valid_message(100);
        assert_eq!(rep.score(), 1);
        assert_eq!(rep.messages_received(), 1);
        assert_eq!(rep.bytes_received(), 100);
    }

    #[test]
    fn invalid_message_decreases_score() {
        let rep = PeerReputation::new();
        rep.record_invalid_message();
        assert_eq!(rep.score(), -10);
        assert_eq!(rep.messages_invalid(), 1);
    }

    #[test]
    fn useful_contribution_increases_score() {
        let rep = PeerReputation::new();
        rep.record_useful_contribution();
        assert_eq!(rep.score(), 5);
    }

    #[test]
    fn violation_decreases_score() {
        let rep = PeerReputation::new();
        rep.record_violation();
        assert_eq!(rep.score(), -50);
    }

    #[test]
    fn score_clamps_at_max() {
        let rep = PeerReputation::new();
        for _ in 0..2000 {
            rep.record_valid_message(1);
        }
        assert_eq!(rep.score(), SCORE_MAX);
    }

    #[test]
    fn score_clamps_at_min() {
        let rep = PeerReputation::new();
        for _ in 0..200 {
            rep.record_violation();
        }
        assert_eq!(rep.score(), SCORE_MIN);
    }

    #[test]
    fn should_disconnect_below_threshold() {
        let rep = PeerReputation::new();
        // 10 violations = -500, but threshold is < -500 so not yet
        for _ in 0..10 {
            rep.record_violation();
        }
        assert_eq!(rep.score(), -500);
        assert!(!rep.should_disconnect());

        // One more pushes past the threshold
        rep.record_violation();
        assert!(rep.should_disconnect());
    }

    #[test]
    fn ping_latency_tracking() {
        let rep = PeerReputation::new();
        rep.record_ping_latency(100);
        rep.record_ping_latency(200);
        assert_eq!(rep.avg_latency_ms(), Some(150));
    }

    #[test]
    fn mixed_behavior_score() {
        let rep = PeerReputation::new();
        // 10 valid messages (+10), 1 invalid (-10), 1 contribution (+5)
        for _ in 0..10 {
            rep.record_valid_message(50);
        }
        rep.record_invalid_message();
        rep.record_useful_contribution();
        assert_eq!(rep.score(), 5); // 10 - 10 + 5
    }

    #[test]
    fn to_json_includes_all_fields() {
        let rep = PeerReputation::new();
        rep.record_valid_message(256);
        rep.record_ping_latency(42);

        let json = rep.to_json();
        assert_eq!(json["score"], 1);
        assert_eq!(json["messages_received"], 1);
        assert_eq!(json["messages_invalid"], 0);
        assert_eq!(json["bytes_received"], 256);
        assert_eq!(json["avg_latency_ms"], 42);
        assert!(json["uptime_secs"].is_number());
    }

    #[test]
    fn uptime_is_tracked() {
        let rep = PeerReputation::new();
        // Uptime should be tracked from construction
        let _ = rep.uptime();
    }
}
