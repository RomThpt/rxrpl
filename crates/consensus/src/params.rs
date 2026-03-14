/// Consensus timing and threshold parameters.
#[derive(Clone, Debug)]
pub struct ConsensusParams {
    /// Minimum time (ms) to keep the ledger open for transactions.
    pub ledger_idle_interval_ms: u64,
    /// Minimum time (ms) between proposals during establish phase.
    pub propose_interval_ms: u64,
    /// Initial threshold percentage for including transactions.
    pub initial_threshold: u32,
    /// Threshold increase per round.
    pub threshold_increase: u32,
    /// Maximum threshold.
    pub max_threshold: u32,
    /// Maximum number of consensus rounds before we agree.
    pub max_consensus_rounds: u32,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            ledger_idle_interval_ms: 15_000,
            propose_interval_ms: 1_250,
            initial_threshold: 50,
            threshold_increase: 10,
            max_threshold: 80,
            max_consensus_rounds: 10,
        }
    }
}

impl ConsensusParams {
    /// Get the threshold for a given round number.
    pub fn threshold_for_round(&self, round: u32) -> u32 {
        let threshold = self.initial_threshold + round * self.threshold_increase;
        threshold.min(self.max_threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_increases() {
        let params = ConsensusParams::default();
        assert_eq!(params.threshold_for_round(0), 50);
        assert_eq!(params.threshold_for_round(1), 60);
        assert_eq!(params.threshold_for_round(2), 70);
        assert_eq!(params.threshold_for_round(3), 80);
        assert_eq!(params.threshold_for_round(4), 80); // capped
    }
}
