/// Adaptive close-time resolution tracker.
///
/// rippled stores close-time resolutions as a fixed ordered set of bin
/// widths (`ledgerPossibleTimeResolutions` in
/// `src/xrpld/consensus/LedgerTiming.h:35-41`): 10s, 20s, 30s, 60s,
/// 90s, 120s.  Index 0 is the FINEST (most precise) bin; the last
/// index is the COARSEST (widest, most slack for clock skew).
///
/// When the prior consensus round agreed on close time, the network
/// can afford to tighten precision — step one slot toward index 0.
/// When it didn't, widen one slot toward the last index so peers with
/// drifting clocks are more likely to round to the same value.
///
/// This tracker keeps the legacy "consecutive agreements" cadence used
/// by `ConsensusEngine`: after `agreements_to_tighten` consecutive
/// rounds of agreement we step one bin finer; any disagreement steps
/// one bin coarser and resets the counter.  T03 will replace the
/// counter pathway with rippled's modulo-on-`ledger_seq` cadence
/// (`getNextLedgerTimeResolution`).
///
/// Reference: rippled `src/xrpld/consensus/LedgerTiming.h:30-122`.

/// Valid close-time-resolution bin widths in seconds, finest to coarsest.
/// Matches rippled `ledgerPossibleTimeResolutions` (LedgerTiming.h:35-41).
pub const TIME_RESOLUTIONS: [u32; 6] = [10, 20, 30, 60, 90, 120];

/// Index into `TIME_RESOLUTIONS` for the default starting resolution
/// (30s). Matches rippled `LedgerDefaultTimeResolution` =
/// `ledgerPossibleTimeResolutions[2]`.
pub const DEFAULT_RESOLUTION_INDEX: usize = 2;

/// Index into `TIME_RESOLUTIONS` for the genesis-ledger resolution
/// (10s). Matches rippled `LedgerGenesisTimeResolution` =
/// `ledgerPossibleTimeResolutions[0]`.
pub const GENESIS_RESOLUTION_INDEX: usize = 0;

/// Default number of consecutive agreements before tightening one bin.
const AGREEMENTS_TO_TIGHTEN: u32 = 5;

/// Tracks consecutive close-time agreements across rounds and adapts the
/// resolution accordingly by stepping through `TIME_RESOLUTIONS`.
#[derive(Clone, Debug)]
pub struct AdaptiveCloseTime {
    /// Current index into `TIME_RESOLUTIONS`.
    index: usize,
    /// Number of consecutive rounds where all validators agreed on close
    /// time (within the current resolution).
    consecutive_agreements: u32,
    /// How many consecutive agreements are needed to step one bin finer.
    agreements_to_tighten: u32,
}

impl AdaptiveCloseTime {
    /// Create a new tracker starting at the resolution closest to
    /// `initial_resolution`.  If `initial_resolution` is one of the
    /// values in `TIME_RESOLUTIONS`, that bin is selected; otherwise
    /// the value is clamped to the nearest valid bin (smaller values
    /// snap to the finest bin, larger to the coarsest).
    pub fn new(initial_resolution: u32) -> Self {
        Self {
            index: nearest_index(initial_resolution),
            consecutive_agreements: 0,
            agreements_to_tighten: AGREEMENTS_TO_TIGHTEN,
        }
    }

    /// Create a tracker starting at the given bin index with a custom
    /// agreements-to-tighten threshold (useful for testing).
    pub fn with_index(index: usize, agreements_to_tighten: u32) -> Self {
        Self {
            index: index.min(TIME_RESOLUTIONS.len() - 1),
            consecutive_agreements: 0,
            agreements_to_tighten,
        }
    }

    /// Current close-time resolution in seconds.
    pub fn resolution(&self) -> u32 {
        TIME_RESOLUTIONS[self.index]
    }

    /// Current bin index into `TIME_RESOLUTIONS`.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Number of consecutive agreements recorded so far.
    pub fn consecutive_agreements(&self) -> u32 {
        self.consecutive_agreements
    }

    /// Record a round where all validators agreed on the close time.
    ///
    /// After `agreements_to_tighten` consecutive agreements the
    /// resolution steps one bin finer (toward index 0).  At the finest
    /// bin the step is refused and the counter still resets.
    pub fn on_agreement(&mut self) {
        self.consecutive_agreements += 1;
        if self.consecutive_agreements >= self.agreements_to_tighten {
            if self.index > 0 {
                self.index -= 1;
            }
            self.consecutive_agreements = 0;
        }
    }

    /// Record a round where validators disagreed on the close time.
    ///
    /// The resolution steps one bin coarser (toward the last index)
    /// and the agreement counter is reset.  At the coarsest bin the
    /// step is refused.
    pub fn on_disagreement(&mut self) {
        self.consecutive_agreements = 0;
        if self.index + 1 < TIME_RESOLUTIONS.len() {
            self.index += 1;
        }
    }

    /// Reset the tracker to the default starting bin.
    pub fn reset(&mut self) {
        self.index = DEFAULT_RESOLUTION_INDEX;
        self.consecutive_agreements = 0;
    }
}

impl Default for AdaptiveCloseTime {
    fn default() -> Self {
        Self::with_index(DEFAULT_RESOLUTION_INDEX, AGREEMENTS_TO_TIGHTEN)
    }
}

/// Snap `resolution` to the closest valid bin index in
/// `TIME_RESOLUTIONS`.  Values smaller than `TIME_RESOLUTIONS[0]` map
/// to index 0; values larger than the last entry map to the last
/// index; values strictly between two bins map to the closer one
/// (ties round down to the finer bin).
fn nearest_index(resolution: u32) -> usize {
    let mut best_idx = 0;
    let mut best_diff = u32::MAX;
    for (i, &r) in TIME_RESOLUTIONS.iter().enumerate() {
        let diff = if r >= resolution {
            r - resolution
        } else {
            resolution - r
        };
        if diff < best_diff {
            best_diff = diff;
            best_idx = i;
        }
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_resolutions_match_rippled_bins() {
        assert_eq!(TIME_RESOLUTIONS, [10, 20, 30, 60, 90, 120]);
        assert_eq!(TIME_RESOLUTIONS[DEFAULT_RESOLUTION_INDEX], 30);
        assert_eq!(TIME_RESOLUTIONS[GENESIS_RESOLUTION_INDEX], 10);
    }

    #[test]
    fn default_starts_at_default_bin() {
        let act = AdaptiveCloseTime::default();
        assert_eq!(act.resolution(), TIME_RESOLUTIONS[DEFAULT_RESOLUTION_INDEX]);
        assert_eq!(act.index(), DEFAULT_RESOLUTION_INDEX);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn tighten_after_5_agreements_steps_one_bin_finer() {
        // Start at index 2 (30s); 5 agreements -> step to index 1 (20s).
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 20);
        assert_eq!(act.index(), 1);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn loosen_on_disagreement_steps_one_bin_coarser() {
        // Start at index 2 (30s); disagreement -> step to index 3 (60s).
        let mut act = AdaptiveCloseTime::new(30);
        act.on_disagreement();
        assert_eq!(act.resolution(), 60);
        assert_eq!(act.index(), 3);
    }

    #[test]
    fn disagreement_resets_agreement_counter() {
        let mut act = AdaptiveCloseTime::new(30);
        act.on_agreement();
        act.on_agreement();
        assert_eq!(act.consecutive_agreements(), 2);
        act.on_disagreement();
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn resolution_pinned_at_finest_bin() {
        // Start at the finest bin (10s, index 0); repeated agreements
        // never step below index 0.
        let mut act = AdaptiveCloseTime::new(10);
        assert_eq!(act.index(), 0);
        for _ in 0..50 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), TIME_RESOLUTIONS[0]);
        assert_eq!(act.index(), 0);
    }

    #[test]
    fn resolution_pinned_at_coarsest_bin() {
        // Start at the coarsest bin (120s); disagreements never step past it.
        let mut act = AdaptiveCloseTime::new(120);
        assert_eq!(act.index(), TIME_RESOLUTIONS.len() - 1);
        act.on_disagreement();
        assert_eq!(act.resolution(), 120);
        act.on_disagreement();
        assert_eq!(act.resolution(), 120);
    }

    #[test]
    fn full_cycle_tighten_then_loosen() {
        // 30 -> 20 (5 agreements) -> 10 (5 more agreements)
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 20);

        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 10);

        // Disagreement: 10 -> 20
        act.on_disagreement();
        assert_eq!(act.resolution(), 20);

        // Disagreement: 20 -> 30
        act.on_disagreement();
        assert_eq!(act.resolution(), 30);

        // Disagreement: 30 -> 60
        act.on_disagreement();
        assert_eq!(act.resolution(), 60);

        // Disagreement: 60 -> 90
        act.on_disagreement();
        assert_eq!(act.resolution(), 90);

        // Disagreement: 90 -> 120
        act.on_disagreement();
        assert_eq!(act.resolution(), 120);

        // Saturated at coarsest.
        act.on_disagreement();
        assert_eq!(act.resolution(), 120);
    }

    #[test]
    fn tighten_walks_all_the_way_down() {
        // From coarsest (120s) walk down to finest (10s) via repeated
        // agreement batches of 5.
        let mut act = AdaptiveCloseTime::new(120);
        let expected = [120, 90, 60, 30, 20, 10, 10, 10];
        for &want in &expected {
            assert_eq!(act.resolution(), want);
            for _ in 0..5 {
                act.on_agreement();
            }
        }
    }

    #[test]
    fn partial_agreements_do_not_tighten() {
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..4 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 30);
    }

    #[test]
    fn custom_threshold_with_index() {
        // Start at index 1 (20s) with a tighten threshold of 3.
        let mut act = AdaptiveCloseTime::with_index(1, 3);
        assert_eq!(act.resolution(), 20);

        // 3 agreements -> step to index 0 (10s).
        for _ in 0..3 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 10);

        // Disagreement -> step to index 1 (20s).
        act.on_disagreement();
        assert_eq!(act.resolution(), 20);

        // Disagreement -> step to index 2 (30s).
        act.on_disagreement();
        assert_eq!(act.resolution(), 30);
    }

    #[test]
    fn reset_restores_default_bin() {
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 20);
        act.reset();
        assert_eq!(act.resolution(), TIME_RESOLUTIONS[DEFAULT_RESOLUTION_INDEX]);
        assert_eq!(act.index(), DEFAULT_RESOLUTION_INDEX);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn initial_resolution_snaps_to_nearest_bin() {
        // Exact bin values snap to themselves.
        for (i, &r) in TIME_RESOLUTIONS.iter().enumerate() {
            assert_eq!(AdaptiveCloseTime::new(r).index(), i);
        }

        // Below the finest bin -> index 0 (10s).
        assert_eq!(AdaptiveCloseTime::new(0).resolution(), 10);
        assert_eq!(AdaptiveCloseTime::new(1).resolution(), 10);
        assert_eq!(AdaptiveCloseTime::new(9).resolution(), 10);

        // Above the coarsest bin -> index 5 (120s).
        assert_eq!(AdaptiveCloseTime::new(200).resolution(), 120);
        assert_eq!(AdaptiveCloseTime::new(u32::MAX).resolution(), 120);

        // Strictly between bins -> closer one.
        assert_eq!(AdaptiveCloseTime::new(15).resolution(), 10); // tie 10/20: rounds down
        assert_eq!(AdaptiveCloseTime::new(16).resolution(), 20);
        assert_eq!(AdaptiveCloseTime::new(45).resolution(), 30); // tie 30/60: rounds down
        assert_eq!(AdaptiveCloseTime::new(46).resolution(), 60);
    }
}
