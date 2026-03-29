use rxrpl_primitives::Hash256;

/// Action to take when consensus stalls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StallAction {
    /// Re-open the same ledger and retry consensus.
    Retry,
    /// Request the latest validated ledger from peers and resync.
    Resync,
}

/// Tracks consensus stall events and determines recovery action.
pub struct StallMetrics {
    total_stalls: u64,
    consecutive_stalls: u32,
    last_stall_ledger: Hash256,
}

const MAX_RETRY_BEFORE_RESYNC: u32 = 3;

impl StallMetrics {
    pub fn new() -> Self {
        Self {
            total_stalls: 0,
            consecutive_stalls: 0,
            last_stall_ledger: Hash256::ZERO,
        }
    }

    /// Record a stall event and return the recommended recovery action.
    pub fn record_stall(&mut self, prev_ledger: &Hash256) -> StallAction {
        self.total_stalls += 1;

        if *prev_ledger == self.last_stall_ledger {
            self.consecutive_stalls += 1;
        } else {
            self.consecutive_stalls = 1;
            self.last_stall_ledger = *prev_ledger;
        }

        if self.consecutive_stalls >= MAX_RETRY_BEFORE_RESYNC {
            StallAction::Resync
        } else {
            StallAction::Retry
        }
    }

    /// Reset consecutive stall counter after a successful consensus round.
    pub fn reset_consecutive(&mut self) {
        self.consecutive_stalls = 0;
        self.last_stall_ledger = Hash256::ZERO;
    }

    pub fn total_stalls(&self) -> u64 {
        self.total_stalls
    }

    pub fn consecutive_stalls(&self) -> u32 {
        self.consecutive_stalls
    }
}

impl Default for StallMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_stall_returns_retry() {
        let mut m = StallMetrics::new();
        let ledger = Hash256::new([0xAA; 32]);
        assert_eq!(m.record_stall(&ledger), StallAction::Retry);
        assert_eq!(m.total_stalls(), 1);
        assert_eq!(m.consecutive_stalls(), 1);
    }

    #[test]
    fn two_consecutive_stalls_still_retry() {
        let mut m = StallMetrics::new();
        let ledger = Hash256::new([0xAA; 32]);
        assert_eq!(m.record_stall(&ledger), StallAction::Retry);
        assert_eq!(m.record_stall(&ledger), StallAction::Retry);
        assert_eq!(m.consecutive_stalls(), 2);
    }

    #[test]
    fn three_consecutive_stalls_trigger_resync() {
        let mut m = StallMetrics::new();
        let ledger = Hash256::new([0xAA; 32]);
        m.record_stall(&ledger);
        m.record_stall(&ledger);
        assert_eq!(m.record_stall(&ledger), StallAction::Resync);
        assert_eq!(m.total_stalls(), 3);
        assert_eq!(m.consecutive_stalls(), 3);
    }

    #[test]
    fn successful_round_resets_consecutive() {
        let mut m = StallMetrics::new();
        let ledger = Hash256::new([0xAA; 32]);
        m.record_stall(&ledger);
        m.record_stall(&ledger);
        m.reset_consecutive();
        assert_eq!(m.consecutive_stalls(), 0);
        assert_eq!(m.record_stall(&ledger), StallAction::Retry);
        assert_eq!(m.consecutive_stalls(), 1);
    }

    #[test]
    fn different_ledger_resets_consecutive() {
        let mut m = StallMetrics::new();
        let l1 = Hash256::new([0xAA; 32]);
        let l2 = Hash256::new([0xBB; 32]);
        m.record_stall(&l1);
        m.record_stall(&l1);
        // Different ledger resets
        assert_eq!(m.record_stall(&l2), StallAction::Retry);
        assert_eq!(m.consecutive_stalls(), 1);
        assert_eq!(m.total_stalls(), 3);
    }

    #[test]
    fn resync_after_different_ledger_sequence() {
        let mut m = StallMetrics::new();
        let l1 = Hash256::new([0xAA; 32]);
        let l2 = Hash256::new([0xBB; 32]);
        // 2 on l1, switch to l2, 3 on l2 -> resync
        m.record_stall(&l1);
        m.record_stall(&l1);
        m.record_stall(&l2);
        m.record_stall(&l2);
        assert_eq!(m.record_stall(&l2), StallAction::Resync);
    }
}
