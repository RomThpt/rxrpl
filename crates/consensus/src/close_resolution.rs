/// Adaptive close-time resolution tracker.
///
/// rippled dynamically adjusts the ledger close-time rounding resolution
/// based on how well validators agree on the close time.  After several
/// consecutive rounds where all validators agreed on the close time
/// (within the current resolution), the resolution is halved to increase
/// precision.  After any disagreement it is doubled back toward the
/// maximum to give the network more slack.
///
/// Boundaries:
/// - Minimum resolution: 1 second
/// - Maximum resolution: 30 seconds (the default starting value)
/// - Agreements required to tighten: 5 consecutive rounds

/// Default number of consecutive agreements before tightening.
const AGREEMENTS_TO_TIGHTEN: u32 = 5;

/// Minimum close-time resolution in seconds.
const MIN_RESOLUTION: u32 = 1;

/// Maximum (and default) close-time resolution in seconds.
const MAX_RESOLUTION: u32 = 30;

/// Tracks consecutive close-time agreements across rounds and adapts the
/// resolution accordingly.
#[derive(Clone, Debug)]
pub struct AdaptiveCloseTime {
    /// Current close-time resolution in seconds.
    resolution: u32,
    /// Number of consecutive rounds where all validators agreed on close
    /// time (within the current resolution).
    consecutive_agreements: u32,
    /// How many consecutive agreements are needed to halve the resolution.
    agreements_to_tighten: u32,
    /// Floor for the resolution.
    min_resolution: u32,
    /// Ceiling for the resolution.
    max_resolution: u32,
}

impl AdaptiveCloseTime {
    /// Create a new tracker with the given initial resolution.
    pub fn new(initial_resolution: u32) -> Self {
        Self {
            resolution: initial_resolution.clamp(MIN_RESOLUTION, MAX_RESOLUTION),
            consecutive_agreements: 0,
            agreements_to_tighten: AGREEMENTS_TO_TIGHTEN,
            min_resolution: MIN_RESOLUTION,
            max_resolution: MAX_RESOLUTION,
        }
    }

    /// Create a tracker with custom bounds (useful for testing).
    pub fn with_bounds(
        initial_resolution: u32,
        min_resolution: u32,
        max_resolution: u32,
        agreements_to_tighten: u32,
    ) -> Self {
        Self {
            resolution: initial_resolution.clamp(min_resolution, max_resolution),
            consecutive_agreements: 0,
            agreements_to_tighten,
            min_resolution,
            max_resolution,
        }
    }

    /// Current close-time resolution in seconds.
    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Number of consecutive agreements recorded so far.
    pub fn consecutive_agreements(&self) -> u32 {
        self.consecutive_agreements
    }

    /// Record a round where all validators agreed on the close time.
    ///
    /// After `agreements_to_tighten` consecutive agreements the resolution
    /// is halved (down to `min_resolution`).
    pub fn on_agreement(&mut self) {
        self.consecutive_agreements += 1;
        if self.consecutive_agreements >= self.agreements_to_tighten {
            self.resolution = (self.resolution / 2).max(self.min_resolution);
            self.consecutive_agreements = 0;
        }
    }

    /// Record a round where validators disagreed on the close time.
    ///
    /// The resolution is doubled (up to `max_resolution`) and the
    /// agreement counter is reset.
    pub fn on_disagreement(&mut self) {
        self.consecutive_agreements = 0;
        self.resolution = (self.resolution * 2).min(self.max_resolution);
    }

    /// Reset the tracker to the maximum resolution.
    pub fn reset(&mut self) {
        self.resolution = self.max_resolution;
        self.consecutive_agreements = 0;
    }
}

impl Default for AdaptiveCloseTime {
    fn default() -> Self {
        Self::new(MAX_RESOLUTION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_starts_at_max_resolution() {
        let act = AdaptiveCloseTime::default();
        assert_eq!(act.resolution(), MAX_RESOLUTION);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn tightens_after_consecutive_agreements() {
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 15);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn disagreement_doubles_resolution() {
        let mut act = AdaptiveCloseTime::new(8);
        act.on_disagreement();
        assert_eq!(act.resolution(), 16);
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
    fn resolution_never_below_minimum() {
        let mut act = AdaptiveCloseTime::new(1);
        // Already at minimum, agreement should keep it at 1
        for _ in 0..10 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), MIN_RESOLUTION);
    }

    #[test]
    fn resolution_never_above_maximum() {
        let mut act = AdaptiveCloseTime::new(30);
        act.on_disagreement();
        assert_eq!(act.resolution(), MAX_RESOLUTION);
        act.on_disagreement();
        assert_eq!(act.resolution(), MAX_RESOLUTION);
    }

    #[test]
    fn full_cycle_tighten_and_loosen() {
        let mut act = AdaptiveCloseTime::new(30);

        // 5 agreements -> 30 halved to 15
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 15);

        // 5 more -> 15 halved to 7
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 7);

        // Disagreement -> 7 doubled to 14
        act.on_disagreement();
        assert_eq!(act.resolution(), 14);

        // Another disagreement -> 14 doubled to 28
        act.on_disagreement();
        assert_eq!(act.resolution(), 28);

        // Another disagreement -> capped at 30
        act.on_disagreement();
        assert_eq!(act.resolution(), MAX_RESOLUTION);
    }

    #[test]
    fn tighten_all_the_way_down() {
        let mut act = AdaptiveCloseTime::new(30);
        // 30 -> 15 -> 7 -> 3 -> 1
        for _ in 0..20 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), MIN_RESOLUTION);
    }

    #[test]
    fn partial_agreements_do_not_tighten() {
        let mut act = AdaptiveCloseTime::new(30);
        // 4 agreements (not enough)
        for _ in 0..4 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 30);
    }

    #[test]
    fn custom_bounds() {
        let mut act = AdaptiveCloseTime::with_bounds(10, 2, 20, 3);
        assert_eq!(act.resolution(), 10);

        // 3 agreements -> halved to 5
        for _ in 0..3 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 5);

        // Disagreement -> doubled to 10
        act.on_disagreement();
        assert_eq!(act.resolution(), 10);

        // Disagreement -> doubled to 20 (max)
        act.on_disagreement();
        assert_eq!(act.resolution(), 20);

        // Disagreement -> capped at 20
        act.on_disagreement();
        assert_eq!(act.resolution(), 20);
    }

    #[test]
    fn reset_restores_max() {
        let mut act = AdaptiveCloseTime::new(30);
        for _ in 0..5 {
            act.on_agreement();
        }
        assert_eq!(act.resolution(), 15);
        act.reset();
        assert_eq!(act.resolution(), MAX_RESOLUTION);
        assert_eq!(act.consecutive_agreements(), 0);
    }

    #[test]
    fn initial_resolution_clamped() {
        let act = AdaptiveCloseTime::new(0);
        assert_eq!(act.resolution(), MIN_RESOLUTION);

        let act = AdaptiveCloseTime::new(100);
        assert_eq!(act.resolution(), MAX_RESOLUTION);
    }
}
