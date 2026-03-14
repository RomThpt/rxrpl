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
/// when the queue is more than half full.
#[derive(Clone, Debug)]
pub struct FeeMetrics {
    /// Current number of transactions in the queue.
    pub queue_size: usize,
    /// Maximum queue capacity.
    pub max_size: usize,
}

impl FeeMetrics {
    pub fn new(max_size: usize) -> Self {
        Self {
            queue_size: 0,
            max_size,
        }
    }

    /// Calculate the escalated fee level based on queue utilization.
    pub fn escalated_fee_level(&self, base_fee_level: u64) -> u64 {
        if self.queue_size <= self.max_size / 2 {
            return base_fee_level;
        }
        // Simple quadratic escalation
        let ratio = self.queue_size as u64 * 256 / self.max_size as u64;
        base_fee_level * ratio / 128
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
        let mut metrics = FeeMetrics::new(100);
        metrics.queue_size = 75;
        let escalated = metrics.escalated_fee_level(256);
        assert!(escalated > 256);
    }
}
