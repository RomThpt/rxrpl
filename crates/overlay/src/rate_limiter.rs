use std::sync::Mutex;
use std::time::Instant;

use rxrpl_p2p_proto::MessageType;

/// Default rate limits (messages per second).
const DEFAULT_GENERAL_RATE: f64 = 100.0;
const DEFAULT_GENERAL_BURST: u32 = 200;

const DEFAULT_TRANSACTION_RATE: f64 = 50.0;
const DEFAULT_TRANSACTION_BURST: u32 = 100;

const DEFAULT_PROPOSAL_RATE: f64 = 20.0;
const DEFAULT_PROPOSAL_BURST: u32 = 40;

const DEFAULT_VALIDATION_RATE: f64 = 20.0;
const DEFAULT_VALIDATION_BURST: u32 = 40;

/// Number of consecutive rate-limit drops before disconnecting a peer.
const DEFAULT_DISCONNECT_THRESHOLD: u32 = 10;

/// Reputation penalty applied when a message is rate-limited.
const RATE_LIMIT_PENALTY: i32 = -5;

/// Configuration for per-peer rate limiting.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Messages per second for general (uncategorized) messages.
    pub general_rate: f64,
    /// Burst capacity for general messages.
    pub general_burst: u32,
    /// Messages per second for transaction messages.
    pub transaction_rate: f64,
    /// Burst capacity for transaction messages.
    pub transaction_burst: u32,
    /// Messages per second for proposal messages.
    pub proposal_rate: f64,
    /// Burst capacity for proposal messages.
    pub proposal_burst: u32,
    /// Messages per second for validation messages.
    pub validation_rate: f64,
    /// Burst capacity for validation messages.
    pub validation_burst: u32,
    /// Number of consecutive drops before triggering a disconnect.
    pub disconnect_threshold: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            general_rate: DEFAULT_GENERAL_RATE,
            general_burst: DEFAULT_GENERAL_BURST,
            transaction_rate: DEFAULT_TRANSACTION_RATE,
            transaction_burst: DEFAULT_TRANSACTION_BURST,
            proposal_rate: DEFAULT_PROPOSAL_RATE,
            proposal_burst: DEFAULT_PROPOSAL_BURST,
            validation_rate: DEFAULT_VALIDATION_RATE,
            validation_burst: DEFAULT_VALIDATION_BURST,
            disconnect_threshold: DEFAULT_DISCONNECT_THRESHOLD,
        }
    }
}

/// A token bucket rate limiter.
///
/// Tokens are added at a fixed rate up to a maximum burst capacity.
/// Each allowed message consumes one token. When no tokens remain,
/// the message is rejected.
#[derive(Debug)]
struct TokenBucket {
    /// Current number of available tokens.
    tokens: f64,
    /// Maximum number of tokens (burst capacity).
    max_tokens: f64,
    /// Tokens added per second.
    rate: f64,
    /// Last time tokens were replenished.
    last_refill: Instant,
}

impl TokenBucket {
    fn new(rate: f64, burst: u32) -> Self {
        Self {
            tokens: burst as f64,
            max_tokens: burst as f64,
            rate,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume one token. Returns true if allowed, false if rate-limited.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Add tokens based on elapsed time since last refill.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.rate).min(self.max_tokens);
            self.last_refill = now;
        }
    }

    /// Reset the bucket to full capacity (used in tests).
    #[cfg(test)]
    fn reset(&mut self) {
        self.tokens = self.max_tokens;
        self.last_refill = Instant::now();
    }
}

/// Result of a rate-limit check.
#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitResult {
    /// Message is allowed.
    Allowed,
    /// Message was dropped due to rate limiting.
    Dropped,
    /// Peer should be disconnected due to sustained abuse.
    Disconnect,
}

/// Per-peer rate limiter with per-message-type token buckets.
///
/// Tracks both per-type and global message rates. Messages must pass
/// both the type-specific bucket and the global bucket to be allowed.
/// Consecutive drops are tracked to trigger disconnection of abusive peers.
pub struct PeerRateLimiter {
    inner: Mutex<PeerRateLimiterInner>,
}

struct PeerRateLimiterInner {
    /// Global rate limiter applied to all messages.
    global: TokenBucket,
    /// Per-type rate limiters for high-volume message types.
    transaction: TokenBucket,
    proposal: TokenBucket,
    validation: TokenBucket,
    /// Number of consecutive rate-limit drops.
    consecutive_drops: u32,
    /// Threshold for triggering disconnect.
    disconnect_threshold: u32,
}

impl std::fmt::Debug for PeerRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerRateLimiter").finish_non_exhaustive()
    }
}

impl PeerRateLimiter {
    pub fn new(config: &RateLimitConfig) -> Self {
        Self {
            inner: Mutex::new(PeerRateLimiterInner {
                global: TokenBucket::new(config.general_rate, config.general_burst),
                transaction: TokenBucket::new(config.transaction_rate, config.transaction_burst),
                proposal: TokenBucket::new(config.proposal_rate, config.proposal_burst),
                validation: TokenBucket::new(config.validation_rate, config.validation_burst),
                consecutive_drops: 0,
                disconnect_threshold: config.disconnect_threshold,
            }),
        }
    }

    /// Check whether a message of the given type should be allowed.
    ///
    /// Returns `Allowed` if both the type-specific and global buckets have
    /// tokens. Returns `Dropped` if rate-limited. Returns `Disconnect` if
    /// the consecutive drop threshold has been exceeded.
    pub fn check(&self, msg_type: MessageType) -> RateLimitResult {
        let mut inner = self.inner.lock().unwrap();

        // Check type-specific bucket first.
        let type_allowed = match msg_type {
            MessageType::Transaction | MessageType::HaveTransactions | MessageType::Transactions => {
                inner.transaction.try_consume()
            }
            MessageType::ProposeSet => inner.proposal.try_consume(),
            MessageType::Validation => inner.validation.try_consume(),
            // Other message types only check the global bucket.
            _ => true,
        };

        // Check global bucket.
        let global_allowed = inner.global.try_consume();

        if type_allowed && global_allowed {
            inner.consecutive_drops = 0;
            RateLimitResult::Allowed
        } else {
            inner.consecutive_drops += 1;
            if inner.consecutive_drops >= inner.disconnect_threshold {
                RateLimitResult::Disconnect
            } else {
                RateLimitResult::Dropped
            }
        }
    }

    /// Get the current number of consecutive drops.
    pub fn consecutive_drops(&self) -> u32 {
        self.inner.lock().unwrap().consecutive_drops
    }

    /// The reputation penalty to apply when a message is rate-limited.
    pub fn penalty() -> i32 {
        RATE_LIMIT_PENALTY
    }
}

impl Default for PeerRateLimiter {
    fn default() -> Self {
        Self::new(&RateLimitConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_bucket_allows_up_to_burst() {
        let mut bucket = TokenBucket::new(10.0, 5);
        for _ in 0..5 {
            assert!(bucket.try_consume());
        }
        // Bucket exhausted.
        assert!(!bucket.try_consume());
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(100.0, 10);
        // Drain the bucket.
        for _ in 0..10 {
            assert!(bucket.try_consume());
        }
        assert!(!bucket.try_consume());

        // Simulate time passing (100 tokens/sec, 50ms = 5 tokens).
        bucket.last_refill = Instant::now() - std::time::Duration::from_millis(50);
        assert!(bucket.try_consume());
    }

    #[test]
    fn token_bucket_does_not_exceed_max() {
        let mut bucket = TokenBucket::new(1000.0, 5);
        // Simulate a long time passing.
        bucket.last_refill = Instant::now() - std::time::Duration::from_secs(10);
        bucket.refill();
        // Tokens should be capped at max_tokens (5).
        assert!(bucket.tokens <= 5.0 + f64::EPSILON);
    }

    #[test]
    fn rate_limiter_allows_normal_traffic() {
        let config = RateLimitConfig::default();
        let limiter = PeerRateLimiter::new(&config);

        // General messages should pass within burst.
        for _ in 0..100 {
            assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Allowed);
        }
    }

    #[test]
    fn rate_limiter_drops_when_exhausted() {
        let config = RateLimitConfig {
            general_rate: 10.0,
            general_burst: 5,
            transaction_rate: 10.0,
            transaction_burst: 5,
            proposal_rate: 10.0,
            proposal_burst: 5,
            validation_rate: 10.0,
            validation_burst: 5,
            disconnect_threshold: 100,
        };
        let limiter = PeerRateLimiter::new(&config);

        // Exhaust the global bucket.
        for _ in 0..5 {
            assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Allowed);
        }
        // Next message should be dropped.
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Dropped);
    }

    #[test]
    fn rate_limiter_per_type_transaction_limit() {
        let config = RateLimitConfig {
            general_rate: 1000.0,
            general_burst: 1000,
            transaction_rate: 5.0,
            transaction_burst: 3,
            proposal_rate: 100.0,
            proposal_burst: 100,
            validation_rate: 100.0,
            validation_burst: 100,
            disconnect_threshold: 100,
        };
        let limiter = PeerRateLimiter::new(&config);

        // Exhaust the transaction bucket.
        for _ in 0..3 {
            assert_eq!(
                limiter.check(MessageType::Transaction),
                RateLimitResult::Allowed,
            );
        }
        // Transaction limit hit, but global still has capacity.
        assert_eq!(
            limiter.check(MessageType::Transaction),
            RateLimitResult::Dropped,
        );
        // Other message types still allowed.
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Allowed);
    }

    #[test]
    fn rate_limiter_per_type_proposal_limit() {
        let config = RateLimitConfig {
            general_rate: 1000.0,
            general_burst: 1000,
            transaction_rate: 100.0,
            transaction_burst: 100,
            proposal_rate: 5.0,
            proposal_burst: 2,
            validation_rate: 100.0,
            validation_burst: 100,
            disconnect_threshold: 100,
        };
        let limiter = PeerRateLimiter::new(&config);

        for _ in 0..2 {
            assert_eq!(
                limiter.check(MessageType::ProposeSet),
                RateLimitResult::Allowed,
            );
        }
        assert_eq!(
            limiter.check(MessageType::ProposeSet),
            RateLimitResult::Dropped,
        );
    }

    #[test]
    fn rate_limiter_per_type_validation_limit() {
        let config = RateLimitConfig {
            general_rate: 1000.0,
            general_burst: 1000,
            transaction_rate: 100.0,
            transaction_burst: 100,
            proposal_rate: 100.0,
            proposal_burst: 100,
            validation_rate: 5.0,
            validation_burst: 2,
            disconnect_threshold: 100,
        };
        let limiter = PeerRateLimiter::new(&config);

        for _ in 0..2 {
            assert_eq!(
                limiter.check(MessageType::Validation),
                RateLimitResult::Allowed,
            );
        }
        assert_eq!(
            limiter.check(MessageType::Validation),
            RateLimitResult::Dropped,
        );
    }

    #[test]
    fn rate_limiter_disconnect_after_sustained_abuse() {
        let config = RateLimitConfig {
            general_rate: 0.0,
            general_burst: 0,
            transaction_rate: 100.0,
            transaction_burst: 100,
            proposal_rate: 100.0,
            proposal_burst: 100,
            validation_rate: 100.0,
            validation_burst: 100,
            disconnect_threshold: 3,
        };
        let limiter = PeerRateLimiter::new(&config);

        // All messages drop because global burst is 0.
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Dropped);
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Dropped);
        // Third consecutive drop triggers disconnect.
        assert_eq!(
            limiter.check(MessageType::Ping),
            RateLimitResult::Disconnect,
        );
    }

    #[test]
    fn rate_limiter_resets_consecutive_drops_on_success() {
        let config = RateLimitConfig {
            general_rate: 10.0,
            general_burst: 3,
            transaction_rate: 10.0,
            transaction_burst: 3,
            proposal_rate: 10.0,
            proposal_burst: 3,
            validation_rate: 10.0,
            validation_burst: 3,
            disconnect_threshold: 5,
        };
        let limiter = PeerRateLimiter::new(&config);

        // Use 3 tokens.
        for _ in 0..3 {
            assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Allowed);
        }
        // Drop two.
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Dropped);
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Dropped);
        assert_eq!(limiter.consecutive_drops(), 2);

        // Simulate time passing to refill.
        {
            let mut inner = limiter.inner.lock().unwrap();
            inner.global.last_refill =
                Instant::now() - std::time::Duration::from_millis(500);
        }

        // Should be allowed again and reset consecutive drops.
        assert_eq!(limiter.check(MessageType::Ping), RateLimitResult::Allowed);
        assert_eq!(limiter.consecutive_drops(), 0);
    }

    #[test]
    fn default_config_values() {
        let config = RateLimitConfig::default();
        assert!((config.general_rate - 100.0).abs() < f64::EPSILON);
        assert_eq!(config.general_burst, 200);
        assert!((config.transaction_rate - 50.0).abs() < f64::EPSILON);
        assert_eq!(config.transaction_burst, 100);
        assert!((config.proposal_rate - 20.0).abs() < f64::EPSILON);
        assert_eq!(config.proposal_burst, 40);
        assert!((config.validation_rate - 20.0).abs() < f64::EPSILON);
        assert_eq!(config.validation_burst, 40);
        assert_eq!(config.disconnect_threshold, 10);
    }

    #[test]
    fn consecutive_drops_accessor() {
        let config = RateLimitConfig {
            general_rate: 0.0,
            general_burst: 0,
            transaction_rate: 100.0,
            transaction_burst: 100,
            proposal_rate: 100.0,
            proposal_burst: 100,
            validation_rate: 100.0,
            validation_burst: 100,
            disconnect_threshold: 100,
        };
        let limiter = PeerRateLimiter::new(&config);

        assert_eq!(limiter.consecutive_drops(), 0);
        limiter.check(MessageType::Ping);
        assert_eq!(limiter.consecutive_drops(), 1);
        limiter.check(MessageType::Ping);
        assert_eq!(limiter.consecutive_drops(), 2);
    }

    #[test]
    fn penalty_value() {
        assert_eq!(PeerRateLimiter::penalty(), -5);
    }
}
