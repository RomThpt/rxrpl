use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::reputation::PeerReputation;

/// Weights for the normalized scoring algorithm.
/// All weights must sum to 100.
const WEIGHT_LATENCY: u32 = 25;
const WEIGHT_UPTIME: u32 = 15;
const WEIGHT_VALIDITY: u32 = 30;
const WEIGHT_RESPONSE_RATE: u32 = 20;
const WEIGHT_BASE_REPUTATION: u32 = 10;

/// Latency thresholds in milliseconds for scoring.
const LATENCY_EXCELLENT_MS: u64 = 50;
const LATENCY_GOOD_MS: u64 = 200;
const LATENCY_ACCEPTABLE_MS: u64 = 500;
const LATENCY_POOR_MS: u64 = 2000;

/// Uptime thresholds for scoring.
const UPTIME_EXCELLENT_SECS: u64 = 3600; // 1 hour
const UPTIME_GOOD_SECS: u64 = 600; // 10 minutes
const UPTIME_MIN_SECS: u64 = 60; // 1 minute

/// Score decay: points removed per decay interval (30s).
const DECAY_AMOUNT: i32 = 1;

/// Minimum number of messages before validity ratio is meaningful.
const MIN_MESSAGES_FOR_VALIDITY: u64 = 10;

/// Peer scoring system that computes a normalized 0-100 score
/// from multiple metrics: latency, uptime, message validity,
/// response rate, and base reputation.
///
/// This normalized score is used for peer selection, preferring
/// well-behaved peers with low latency and high uptime.
pub struct PeerScore {
    /// Number of requests sent to this peer.
    requests_sent: AtomicU64,
    /// Number of responses received from this peer.
    responses_received: AtomicU64,
    /// Number of request timeouts for this peer.
    timeouts: AtomicU64,
    /// Timestamp of the last decay application.
    last_decay: std::sync::Mutex<Instant>,
}

impl std::fmt::Debug for PeerScore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerScore")
            .field("requests_sent", &self.requests_sent.load(Ordering::Relaxed))
            .field(
                "responses_received",
                &self.responses_received.load(Ordering::Relaxed),
            )
            .field("timeouts", &self.timeouts.load(Ordering::Relaxed))
            .finish()
    }
}

impl PeerScore {
    pub fn new() -> Self {
        Self {
            requests_sent: AtomicU64::new(0),
            responses_received: AtomicU64::new(0),
            timeouts: AtomicU64::new(0),
            last_decay: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Record that a request was sent to this peer.
    pub fn record_request_sent(&self) {
        self.requests_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Record that a response was received from this peer.
    pub fn record_response_received(&self) {
        self.responses_received.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a request timeout for this peer.
    pub fn record_timeout(&self) {
        self.timeouts.fetch_add(1, Ordering::Relaxed);
    }

    /// Get the number of requests sent.
    pub fn requests_sent(&self) -> u64 {
        self.requests_sent.load(Ordering::Relaxed)
    }

    /// Get the number of responses received.
    pub fn responses_received(&self) -> u64 {
        self.responses_received.load(Ordering::Relaxed)
    }

    /// Get the number of timeouts.
    pub fn timeouts(&self) -> u64 {
        self.timeouts.load(Ordering::Relaxed)
    }

    /// Response rate as a value between 0.0 and 1.0.
    /// Returns 1.0 if no requests have been sent yet (benefit of the doubt).
    pub fn response_rate(&self) -> f64 {
        let sent = self.requests_sent.load(Ordering::Relaxed);
        if sent == 0 {
            return 1.0;
        }
        let received = self.responses_received.load(Ordering::Relaxed);
        (received as f64) / (sent as f64)
    }

    /// Apply temporal decay to the reputation score.
    /// Should be called periodically (e.g., every 30 seconds).
    /// Decays positive scores toward zero to ensure peers must
    /// continuously contribute to maintain their standing.
    pub fn apply_decay(&self, reputation: &PeerReputation) {
        let mut last = self.last_decay.lock().unwrap();
        let elapsed = last.elapsed();
        let intervals = (elapsed.as_secs() / 30) as i32;
        if intervals > 0 {
            let current_score = reputation.score();
            if current_score > 0 {
                let decay = (DECAY_AMOUNT * intervals).min(current_score);
                reputation.apply_penalty(decay);
            }
            *last = Instant::now();
        }
    }

    /// Compute a normalized score from 0 to 100 based on multiple metrics.
    ///
    /// Components (weights summing to 100):
    /// - Latency (25): lower is better, scored 0-100
    /// - Uptime (15): longer is better, scored 0-100
    /// - Validity ratio (30): fewer invalid messages is better
    /// - Response rate (20): higher response rate is better
    /// - Base reputation (10): raw reputation score normalized
    pub fn normalized_score(&self, reputation: &PeerReputation) -> u32 {
        let latency_score = self.score_latency(reputation);
        let uptime_score = self.score_uptime(reputation);
        let validity_score = self.score_validity(reputation);
        let response_score = self.score_response_rate();
        let base_score = self.score_base_reputation(reputation);

        let weighted = latency_score * WEIGHT_LATENCY
            + uptime_score * WEIGHT_UPTIME
            + validity_score * WEIGHT_VALIDITY
            + response_score * WEIGHT_RESPONSE_RATE
            + base_score * WEIGHT_BASE_REPUTATION;

        (weighted / 100).min(100)
    }

    /// Score latency component (0-100).
    fn score_latency(&self, reputation: &PeerReputation) -> u32 {
        match reputation.avg_latency_ms() {
            None => 50, // No data yet, neutral score
            Some(ms) if ms <= LATENCY_EXCELLENT_MS => 100,
            Some(ms) if ms <= LATENCY_GOOD_MS => {
                let range = LATENCY_GOOD_MS - LATENCY_EXCELLENT_MS;
                let offset = ms - LATENCY_EXCELLENT_MS;
                100 - ((offset * 25) / range) as u32
            }
            Some(ms) if ms <= LATENCY_ACCEPTABLE_MS => {
                let range = LATENCY_ACCEPTABLE_MS - LATENCY_GOOD_MS;
                let offset = ms - LATENCY_GOOD_MS;
                75 - ((offset * 25) / range) as u32
            }
            Some(ms) if ms <= LATENCY_POOR_MS => {
                let range = LATENCY_POOR_MS - LATENCY_ACCEPTABLE_MS;
                let offset = ms - LATENCY_ACCEPTABLE_MS;
                50 - ((offset * 30) / range) as u32
            }
            Some(_) => 10, // Very high latency
        }
    }

    /// Score uptime component (0-100).
    fn score_uptime(&self, reputation: &PeerReputation) -> u32 {
        let secs = reputation.uptime().as_secs();
        if secs >= UPTIME_EXCELLENT_SECS {
            100
        } else if secs >= UPTIME_GOOD_SECS {
            let range = UPTIME_EXCELLENT_SECS - UPTIME_GOOD_SECS;
            let offset = secs - UPTIME_GOOD_SECS;
            75 + ((offset * 25) / range) as u32
        } else if secs >= UPTIME_MIN_SECS {
            let range = UPTIME_GOOD_SECS - UPTIME_MIN_SECS;
            let offset = secs - UPTIME_MIN_SECS;
            25 + ((offset * 50) / range) as u32
        } else {
            (secs * 25 / UPTIME_MIN_SECS.max(1)) as u32
        }
    }

    /// Score message validity ratio (0-100).
    fn score_validity(&self, reputation: &PeerReputation) -> u32 {
        let total = reputation.messages_received();
        let invalid = reputation.messages_invalid();

        if total < MIN_MESSAGES_FOR_VALIDITY {
            return 70; // Not enough data, slightly positive assumption
        }

        let valid = total.saturating_sub(invalid);
        let ratio = (valid as f64) / (total as f64);

        // Map ratio to score: 1.0 -> 100, 0.95 -> 80, 0.9 -> 50, <0.8 -> 0
        if ratio >= 1.0 {
            100
        } else if ratio >= 0.95 {
            let t = (ratio - 0.95) / 0.05;
            80 + (t * 20.0) as u32
        } else if ratio >= 0.9 {
            let t = (ratio - 0.9) / 0.05;
            50 + (t * 30.0) as u32
        } else if ratio >= 0.8 {
            let t = (ratio - 0.8) / 0.1;
            (t * 50.0) as u32
        } else {
            0
        }
    }

    /// Score response rate (0-100).
    fn score_response_rate(&self) -> u32 {
        let rate = self.response_rate();
        (rate * 100.0).round() as u32
    }

    /// Score base reputation (0-100), mapping [-1000, 1000] to [0, 100].
    fn score_base_reputation(&self, reputation: &PeerReputation) -> u32 {
        let score = reputation.score();
        // Map -1000..1000 to 0..100
        let normalized = ((score + 1000) as u32 * 100) / 2000;
        normalized.min(100)
    }

    /// Get a JSON summary for diagnostics.
    pub fn to_json(&self, reputation: &PeerReputation) -> serde_json::Value {
        serde_json::json!({
            "normalized_score": self.normalized_score(reputation),
            "requests_sent": self.requests_sent(),
            "responses_received": self.responses_received(),
            "timeouts": self.timeouts(),
            "response_rate": self.response_rate(),
        })
    }
}

impl Default for PeerScore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_reputation() -> PeerReputation {
        PeerReputation::new()
    }

    #[test]
    fn new_score_defaults() {
        let score = PeerScore::new();
        assert_eq!(score.requests_sent(), 0);
        assert_eq!(score.responses_received(), 0);
        assert_eq!(score.timeouts(), 0);
        assert!((score.response_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn response_rate_calculation() {
        let score = PeerScore::new();
        score.record_request_sent();
        score.record_request_sent();
        score.record_response_received();
        assert!((score.response_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn response_rate_no_requests() {
        let score = PeerScore::new();
        assert!((score.response_rate() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn timeout_tracking() {
        let score = PeerScore::new();
        score.record_timeout();
        score.record_timeout();
        assert_eq!(score.timeouts(), 2);
    }

    #[test]
    fn normalized_score_new_peer() {
        let score = PeerScore::new();
        let rep = make_reputation();
        let ns = score.normalized_score(&rep);
        // New peer: latency=50, uptime=low, validity=70, response=100, base=50
        // Should be moderate
        assert!(ns > 0 && ns <= 100);
    }

    #[test]
    fn normalized_score_excellent_peer() {
        let score = PeerScore::new();
        let rep = make_reputation();

        // Simulate good behavior: many valid messages
        for _ in 0..100 {
            rep.record_valid_message(100);
        }
        rep.record_ping_latency(30); // Excellent latency

        // Good response rate
        for _ in 0..10 {
            score.record_request_sent();
            score.record_response_received();
        }

        let ns = score.normalized_score(&rep);
        assert!(ns >= 50, "excellent peer should score high, got {}", ns);
    }

    #[test]
    fn normalized_score_bad_peer() {
        let score = PeerScore::new();
        let rep = make_reputation();

        // Many invalid messages
        for _ in 0..50 {
            rep.record_invalid_message();
        }
        // Poor response rate
        for _ in 0..10 {
            score.record_request_sent();
        }
        // Only 2 responses out of 10
        score.record_response_received();
        score.record_response_received();

        let ns = score.normalized_score(&rep);
        assert!(ns < 50, "bad peer should score low, got {}", ns);
    }

    #[test]
    fn latency_scoring_tiers() {
        let score = PeerScore::new();
        let rep = make_reputation();

        // Excellent latency
        rep.record_ping_latency(30);
        assert_eq!(score.score_latency(&rep), 100);
    }

    #[test]
    fn latency_scoring_good() {
        let score = PeerScore::new();
        let rep = make_reputation();
        rep.record_ping_latency(100);
        let ls = score.score_latency(&rep);
        assert!(ls >= 75 && ls <= 100, "good latency score should be 75-100, got {}", ls);
    }

    #[test]
    fn latency_scoring_poor() {
        let score = PeerScore::new();
        let rep = make_reputation();
        rep.record_ping_latency(3000);
        let ls = score.score_latency(&rep);
        assert_eq!(ls, 10);
    }

    #[test]
    fn latency_scoring_no_data() {
        let score = PeerScore::new();
        let rep = make_reputation();
        assert_eq!(score.score_latency(&rep), 50);
    }

    #[test]
    fn validity_scoring_perfect() {
        let score = PeerScore::new();
        let rep = make_reputation();
        for _ in 0..20 {
            rep.record_valid_message(100);
        }
        assert_eq!(score.score_validity(&rep), 100);
    }

    #[test]
    fn validity_scoring_insufficient_data() {
        let score = PeerScore::new();
        let rep = make_reputation();
        rep.record_valid_message(100);
        assert_eq!(score.score_validity(&rep), 70);
    }

    #[test]
    fn validity_scoring_mixed() {
        let score = PeerScore::new();
        let rep = make_reputation();
        // 18 valid + 2 invalid = 20 total, ratio = 0.9
        for _ in 0..18 {
            rep.record_valid_message(100);
        }
        rep.record_invalid_message();
        rep.record_invalid_message();
        let vs = score.score_validity(&rep);
        // messages_received = 18, messages_invalid = 2
        // But record_valid_message increments messages_received,
        // record_invalid_message increments messages_invalid but NOT messages_received.
        // So total received = 18, invalid = 2 -> valid = 16, ratio = 16/18 = 0.89
        // This falls in the 0.8-0.9 range
        assert!(vs < 70, "mixed validity should score below 70, got {}", vs);
    }

    #[test]
    fn base_reputation_scoring() {
        let score = PeerScore::new();
        let rep = make_reputation();
        // score = 0 -> normalized = (0 + 1000) * 100 / 2000 = 50
        assert_eq!(score.score_base_reputation(&rep), 50);
    }

    #[test]
    fn base_reputation_scoring_high() {
        let score = PeerScore::new();
        let rep = make_reputation();
        for _ in 0..500 {
            rep.record_valid_message(1);
        }
        let bs = score.score_base_reputation(&rep);
        // score = 500 -> (500 + 1000) * 100 / 2000 = 75
        assert_eq!(bs, 75);
    }

    #[test]
    fn decay_does_not_affect_zero_score() {
        let score = PeerScore::new();
        let rep = make_reputation();
        // Force last_decay to be old
        {
            let mut last = score.last_decay.lock().unwrap();
            *last = Instant::now() - std::time::Duration::from_secs(60);
        }
        score.apply_decay(&rep);
        assert_eq!(rep.score(), 0);
    }

    #[test]
    fn decay_reduces_positive_score() {
        let score = PeerScore::new();
        let rep = make_reputation();
        // Build up score
        for _ in 0..10 {
            rep.record_valid_message(1);
        }
        assert_eq!(rep.score(), 10);

        // Force last_decay to be 60 seconds ago (2 intervals)
        {
            let mut last = score.last_decay.lock().unwrap();
            *last = Instant::now() - std::time::Duration::from_secs(60);
        }
        score.apply_decay(&rep);
        assert_eq!(rep.score(), 8); // 10 - (1 * 2)
    }

    #[test]
    fn to_json_includes_all_fields() {
        let score = PeerScore::new();
        let rep = make_reputation();
        score.record_request_sent();
        score.record_response_received();

        let json = score.to_json(&rep);
        assert!(json["normalized_score"].is_number());
        assert_eq!(json["requests_sent"], 1);
        assert_eq!(json["responses_received"], 1);
        assert_eq!(json["timeouts"], 0);
        assert!(json["response_rate"].is_number());
    }

    #[test]
    fn uptime_scoring_zero() {
        let score = PeerScore::new();
        let rep = make_reputation();
        // Just created, uptime ~ 0
        let us = score.score_uptime(&rep);
        // Should be very low
        assert!(us <= 25);
    }

    #[test]
    fn response_rate_score_perfect() {
        let score = PeerScore::new();
        for _ in 0..5 {
            score.record_request_sent();
            score.record_response_received();
        }
        assert_eq!(score.score_response_rate(), 100);
    }

    #[test]
    fn response_rate_score_zero() {
        let score = PeerScore::new();
        for _ in 0..5 {
            score.record_request_sent();
        }
        assert_eq!(score.score_response_rate(), 0);
    }
}
