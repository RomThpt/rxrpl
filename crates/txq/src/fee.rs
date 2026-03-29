/// Fee level for queue ordering.
///
/// Transactions are ordered by fee level (fee / base_fee),
/// with higher fee levels processed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FeeLevel(u64);

impl FeeLevel {
    /// Calculate fee level from fee and base fee.
    pub fn new(fee_drops: u64, base_fee: u64) -> Self {
        if base_fee == 0 {
            return Self(fee_drops);
        }
        Self(fee_drops * 256 / base_fee)
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

/// Fee escalation metrics.
///
/// Tracks queue utilization to calculate escalated fees
/// when the queue is more than half full (matching rippled behavior).
#[derive(Clone, Debug)]
pub struct FeeMetrics {
    /// Current number of transactions in the queue.
    pub queue_size: usize,
    /// Maximum queue capacity.
    pub max_size: usize,
}

/// Base fee level for a reference transaction (256 = 1x multiplier).
pub const BASE_FEE_LEVEL: u64 = 256;

/// Maximum number of queued transactions per account (matches rippled).
pub const MAX_ACCOUNT_QUEUE_DEPTH: usize = 10;

impl FeeMetrics {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue_size: 0,
            max_size,
        }
    }

    /// Build metrics from a live queue snapshot.
    pub fn from_queue(queue_size: usize, max_size: usize) -> Self {
        Self {
            queue_size,
            max_size,
        }
    }

    /// Calculate the escalated fee level based on queue utilization.
    ///
    /// When the queue is at most half full the base fee level is returned.
    /// Above 50% capacity a quadratic escalation kicks in, matching
    /// rippled's `escalatedSerializedSize` formula.
    pub fn escalated_fee_level(&self, base_fee_level: u64) -> u64 {
        if self.max_size == 0 || self.queue_size <= self.max_size / 2 {
            return base_fee_level;
        }
        // Simple quadratic escalation
        let ratio = self.queue_size as u64 * 256 / self.max_size as u64;
        base_fee_level * ratio / 128
    }

    /// Convert an escalated fee level back to drops for a given base fee.
    ///
    /// This is the inverse of `FeeLevel::new`: fee_drops = level * base_fee / 256.
    pub fn escalated_fee_drops(&self, base_fee: u64) -> u64 {
        let level = self.escalated_fee_level(BASE_FEE_LEVEL);
        // Round up so the user always pays enough
        (level * base_fee + 255) / 256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_level_calculation() {
        assert_eq!(FeeLevel::new(10, 10).value(), 256);
        assert_eq!(FeeLevel::new(20, 10).value(), 512);
        assert_eq!(FeeLevel::new(5, 10).value(), 128);
    }

    #[test]
    fn fee_level_ordering() {
        let low = FeeLevel::new(10, 10);
        let high = FeeLevel::new(20, 10);
        assert!(high > low);
    }

    #[test]
    fn no_escalation_below_half() {
        let metrics = FeeMetrics::new(100);
        assert_eq!(metrics.escalated_fee_level(256), 256);
    }

    #[test]
    fn escalation_above_half() {
        let metrics = FeeMetrics::from_queue(75, 100);
        let escalated = metrics.escalated_fee_level(256);
        assert!(escalated > 256);
    }

    #[test]
    fn escalated_fee_drops_at_base() {
        // When queue is empty the escalated fee equals the base fee.
        let metrics = FeeMetrics::new(100);
        assert_eq!(metrics.escalated_fee_drops(10), 10);
    }

    #[test]
    fn escalated_fee_drops_above_half() {
        let metrics = FeeMetrics::from_queue(75, 100);
        let drops = metrics.escalated_fee_drops(10);
        assert!(drops > 10, "expected escalated drops > 10, got {drops}");
    }

    #[test]
    fn escalation_zero_max_size() {
        // Guard against division by zero.
        let metrics = FeeMetrics::from_queue(0, 0);
        assert_eq!(metrics.escalated_fee_level(256), 256);
        assert_eq!(metrics.escalated_fee_drops(10), 10);
    }

    #[test]
    fn from_queue_sets_fields() {
        let m = FeeMetrics::from_queue(42, 200);
        assert_eq!(m.queue_size, 42);
        assert_eq!(m.max_size, 200);
    }
}
