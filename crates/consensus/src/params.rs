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
    /// Close time rounding resolution in seconds.
    pub close_time_resolution: u32,
}

impl Default for ConsensusParams {
    fn default() -> Self {
        Self {
            // 5s open phase for fast rounds (~17s/round = 5s open + ~12s establish
            // max). Cross-impl bootstrap protected separately by the "wait for
            // first peer status" gate in close_consensus_round — without that,
            // 5s would close before peer connects (~17s to first StatusChange)
            // and #2 would diverge. With both fixes: bootstrap waits for peer,
            // then steady-state runs at rippled's pace so rxrpl can produce
            // validations fast enough for rippled to advance under quorum=2.
            ledger_idle_interval_ms: 2_000,
            propose_interval_ms: 1_250,
            initial_threshold: 50,
            threshold_increase: 10,
            max_threshold: 80,
            // Establish window = propose_interval_ms * max_consensus_rounds
            // = 1.25s * 25 = ~31s. Must exceed rippled-2.6.2's `idle_interval`
            // of 20s so rxrpl waits long enough to observe rippled's first
            // ProposeSet of the round before falling through to force-accept.
            // The engine's `peer_positions.is_empty()` guard prevents fork-on-
            // timeout in the UNL case; this raises the natural window so the
            // guard is rarely needed in practice.
            max_consensus_rounds: 25,
            close_time_resolution: 30,
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
