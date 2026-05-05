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
/// As of T03 the tracker is driven exclusively through
/// [`set_resolution`], which the engine recomputes once per round via
/// [`next_resolution`] using the rippled modulo-on-`ledger_seq`
/// cadence (`getNextLedgerTimeResolution`).  The legacy
/// consecutive-agreements pathway ([`AdaptiveCloseTime::on_agreement`]
/// / [`AdaptiveCloseTime::on_disagreement`]) is preserved as
/// `#[deprecated]` no-ops for callers still on the old API; it is no
/// longer wired into [`crate::ConsensusEngine`].
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

/// Modulo cadence: every N ledgers, if the prior round agreed, try to
/// step to a FINER bin (smaller seconds).  Matches rippled
/// `increaseLedgerTimeResolutionEvery` (LedgerTiming.h:30).
pub const INCREASE_LEDGER_TIME_RESOLUTION_EVERY: u32 = 8;

/// Modulo cadence: every N ledgers, if the prior round did NOT agree,
/// try to step to a COARSER bin (larger seconds).  Matches rippled
/// `decreaseLedgerTimeResolutionEvery` (LedgerTiming.h:33).
pub const DECREASE_LEDGER_TIME_RESOLUTION_EVERY: u32 = 1;

/// Compute the close-time resolution (in seconds) for the ledger at
/// sequence `new_ledger_seq`, given the parent ledger's resolution and
/// whether the prior consensus round agreed on close time.
///
/// Mirrors rippled `getNextLedgerTimeResolution`
/// (`src/xrpld/consensus/LedgerTiming.h:60-98`).  The cadence is keyed
/// on `new_ledger_seq` modulo the rippled constants
/// `decreaseLedgerTimeResolutionEvery` (1) and
/// `increaseLedgerTimeResolutionEvery` (8) — NOT on a count of
/// consecutive agreements.  The two paths are intentionally checked in
/// order: if a prior disagreement triggers a coarsening step, that
/// result is returned before the agreement branch is even considered.
///
/// Contract:
/// - `parent_resolution` MUST be one of the values in
///   [`TIME_RESOLUTIONS`].  If it isn't (corruption or programmer
///   error), `parent_resolution` is returned unchanged — matching the
///   rippled "precaution" branch (LedgerTiming.h:78-79).
/// - `new_ledger_seq` MUST be non-zero.  Rippled asserts this; here we
///   tolerate `0` by returning `parent_resolution` so the helper stays
///   pure.  Real callers always pass `seq >= 2` (genesis is `seq == 1`
///   and never re-enters this function).
///
/// Note on terminology: rippled comments use "increase resolution" in
/// the human sense (finer → smaller seconds), while the bin array is
/// sorted by seconds-per-bin ascending, so "finer" means moving to a
/// SMALLER index.  The constant names are preserved verbatim from
/// rippled so cross-references to LedgerTiming.h remain unambiguous.
//
// `modulo_one` is intentional here: rippled defines
// `decreaseLedgerTimeResolutionEvery = 1` as a tunable cadence
// constant.  Keeping the `% DECREASE_..._EVERY` expression makes the
// rippled parity obvious and lets the constant be tuned without
// rewriting the function body.
//
// `collapsible_if` is intentional here: the inner `if` corresponds to
// rippled's `if (++iter != end)` / `if (iter-- != begin)` saturation
// guard.  Keeping the two `if`s separate preserves the one-to-one
// mapping with LedgerTiming.h:83-95.
#[allow(clippy::modulo_one, clippy::collapsible_if)]
pub fn next_resolution(parent_resolution: u32, previous_agree: bool, new_ledger_seq: u32) -> u32 {
    if new_ledger_seq == 0 {
        return parent_resolution;
    }

    // Locate parent_resolution in the bin array.  Mirrors the
    // `std::find` lookup in LedgerTiming.h:69-72.
    let idx = match TIME_RESOLUTIONS
        .iter()
        .position(|&r| r == parent_resolution)
    {
        Some(i) => i,
        // Precaution branch (LedgerTiming.h:78-79): unknown bin → no-op.
        None => return parent_resolution,
    };

    // Prior round did NOT agree: try to step COARSER (larger bin) so
    // peers with drifting clocks are more likely to round to the same
    // value.  Mirrors LedgerTiming.h:83-87.
    if !previous_agree && new_ledger_seq % DECREASE_LEDGER_TIME_RESOLUTION_EVERY == 0 {
        if idx + 1 < TIME_RESOLUTIONS.len() {
            return TIME_RESOLUTIONS[idx + 1];
        }
    }

    // Prior round DID agree: try to step FINER (smaller bin) to see if
    // the network can keep agreeing at higher precision.  Mirrors
    // LedgerTiming.h:91-95.  At idx 0 (already finest) the step is
    // refused and we fall through to return `parent_resolution`.
    if previous_agree && new_ledger_seq % INCREASE_LEDGER_TIME_RESOLUTION_EVERY == 0 {
        if idx > 0 {
            return TIME_RESOLUTIONS[idx - 1];
        }
    }

    parent_resolution
}

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
    ///
    /// Maintained only by the deprecated [`Self::on_agreement`] /
    /// [`Self::on_disagreement`] pathway.  The current engine drives
    /// the tracker through [`Self::set_resolution`] and never touches
    /// this counter; callers that have migrated should ignore it.
    pub fn consecutive_agreements(&self) -> u32 {
        self.consecutive_agreements
    }

    /// Snap the tracker to the bin nearest to `resolution` (using the
    /// same ties-round-down rule as [`Self::new`]).  This is the API
    /// the engine uses each round once it has computed the next
    /// resolution via [`next_resolution`].
    pub fn set_resolution(&mut self, resolution: u32) {
        self.index = nearest_index(resolution);
    }

    /// Record a round where all validators agreed on the close time.
    ///
    /// **Deprecated.** The engine no longer steps the bin from this
    /// hook; resolution is recomputed each round via
    /// [`next_resolution`] and applied through [`Self::set_resolution`].
    /// The method is preserved as an in-place no-op for backwards
    /// compatibility with callers on the old consecutive-counter API.
    #[deprecated(
        note = "use next_resolution + set_resolution; consecutive-agreements pathway removed in T03"
    )]
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
    /// **Deprecated.** The engine no longer steps the bin from this
    /// hook; resolution is recomputed each round via
    /// [`next_resolution`] and applied through [`Self::set_resolution`].
    /// The method is preserved for backwards compatibility with callers
    /// on the old consecutive-counter API.
    #[deprecated(
        note = "use next_resolution + set_resolution; consecutive-agreements pathway removed in T03"
    )]
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
#[allow(deprecated)] // exercises the legacy on_agreement/on_disagreement API kept for back-compat
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

    // ---------------------------------------------------------------
    // next_resolution: rippled getNextLedgerTimeResolution parity tests
    // (LedgerTiming.h:60-98).  These exercise the modulo-on-ledger_seq
    // cadence — distinct from the consecutive-agreements path used by
    // AdaptiveCloseTime above.
    // ---------------------------------------------------------------

    #[test]
    fn next_resolution_seq_zero_returns_parent_unchanged() {
        // Rippled asserts on seq == 0; we silently no-op.  Verify both
        // branches (agree / disagree) return the parent regardless.
        assert_eq!(next_resolution(30, true, 0), 30);
        assert_eq!(next_resolution(30, false, 0), 30);
        assert_eq!(next_resolution(120, true, 0), 120);
        assert_eq!(next_resolution(10, false, 0), 10);
    }

    #[test]
    fn next_resolution_disagree_at_coarsest_bin_saturates() {
        // idx 5 (120s) is the coarsest bin; !previous_agree wants to
        // step coarser but idx+1 == len, so the parent is returned.
        // Try a handful of seq values to confirm saturation is sticky.
        for seq in [1u32, 2, 3, 7, 8, 9, 100, 12_345] {
            assert_eq!(
                next_resolution(120, false, seq),
                120,
                "seq={seq} should saturate at 120s"
            );
        }
    }

    #[test]
    fn next_resolution_disagree_at_30s_steps_to_60s() {
        // idx 2 (30s) → !previous_agree && seq % 1 == 0 (always) → idx 3 (60s).
        // Verify the step fires for every seq, since the modulus is 1.
        for seq in [1u32, 2, 3, 7, 8, 9, 17, 100] {
            assert_eq!(
                next_resolution(30, false, seq),
                60,
                "seq={seq} should step 30s → 60s on disagreement"
            );
        }
    }

    #[test]
    fn next_resolution_agree_at_seq_8_steps_30s_to_20s() {
        // idx 2 (30s), previous_agree=true, seq=8 → seq % 8 == 0 →
        // step finer to idx 1 (20s).  Also verify the step at multiples
        // of 8 (16, 24, 800).
        assert_eq!(next_resolution(30, true, 8), 20);
        assert_eq!(next_resolution(30, true, 16), 20);
        assert_eq!(next_resolution(30, true, 24), 20);
        assert_eq!(next_resolution(30, true, 800), 20);
    }

    #[test]
    fn next_resolution_agree_at_seq_not_multiple_of_8_keeps_parent() {
        // idx 2 (30s), previous_agree=true, seq % 8 != 0 → no step.
        // Cover all non-zero residues mod 8.
        for seq in [1u32, 2, 3, 4, 5, 6, 7, 9, 10, 11, 15, 17, 23] {
            assert_eq!(
                next_resolution(30, true, seq),
                30,
                "seq={seq} (mod 8 != 0) should keep 30s on agreement"
            );
        }
    }

    #[test]
    fn next_resolution_agree_at_finest_bin_saturates() {
        // idx 0 (10s) is the finest; previous_agree at seq % 8 == 0
        // would want to step finer but idx == 0 so the parent is kept.
        assert_eq!(next_resolution(10, true, 8), 10);
        assert_eq!(next_resolution(10, true, 16), 10);
        assert_eq!(next_resolution(10, true, 64), 10);
        // Non-multiple of 8 also keeps it (modulo gate fails first).
        assert_eq!(next_resolution(10, true, 7), 10);
    }

    #[test]
    fn next_resolution_unknown_parent_bin_returns_unchanged() {
        // Precaution branch (LedgerTiming.h:78-79): a parent
        // resolution that is not a member of TIME_RESOLUTIONS is
        // echoed back verbatim, no matter the seq or agree flag.
        assert_eq!(next_resolution(0, true, 8), 0);
        assert_eq!(next_resolution(15, false, 1), 15);
        assert_eq!(next_resolution(45, true, 16), 45);
        assert_eq!(next_resolution(121, false, 1), 121);
        assert_eq!(next_resolution(u32::MAX, true, 8), u32::MAX);
    }

    #[test]
    fn next_resolution_disagree_walks_all_bins_up_to_120() {
        // From idx 0 (10s), repeated disagreements step one coarser
        // each call (seq % 1 == 0 always), saturating at 120s.
        let walk = [
            (10, 20),
            (20, 30),
            (30, 60),
            (60, 90),
            (90, 120),
            (120, 120),
        ];
        for (parent, want) in walk {
            assert_eq!(
                next_resolution(parent, false, 1),
                want,
                "{parent}s on disagreement should step to {want}s"
            );
        }
    }

    #[test]
    fn next_resolution_agree_walks_all_bins_down_to_10() {
        // From idx 5 (120s), repeated agreements at seq % 8 == 0 step
        // one finer each call, saturating at 10s.
        let walk = [(120, 90), (90, 60), (60, 30), (30, 20), (20, 10), (10, 10)];
        for (parent, want) in walk {
            assert_eq!(
                next_resolution(parent, true, 8),
                want,
                "{parent}s on agreement at seq=8 should step to {want}s"
            );
        }
    }

    #[test]
    fn next_resolution_disagree_branch_takes_precedence_over_agree_check() {
        // Sanity: at seq=8 the agree branch *would* fire, but with
        // previous_agree=false only the disagree branch is even
        // checked.  Confirms the two branches are mutually exclusive
        // by virtue of the `previous_agree` flag.
        assert_eq!(next_resolution(30, false, 8), 60);
        assert_eq!(next_resolution(30, true, 8), 20);
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

    #[test]
    fn set_resolution_snaps_to_nearest_bin() {
        // Exact bin values land on themselves.
        let mut act = AdaptiveCloseTime::new(30);
        for (i, &r) in TIME_RESOLUTIONS.iter().enumerate() {
            act.set_resolution(r);
            assert_eq!(act.index(), i, "exact bin {r} should map to index {i}");
            assert_eq!(act.resolution(), r);
        }

        // Out-of-band values clamp to the nearest bin (same rule as
        // `new`): below finest snaps to 10s, above coarsest to 120s.
        act.set_resolution(0);
        assert_eq!(act.resolution(), 10);
        act.set_resolution(u32::MAX);
        assert_eq!(act.resolution(), 120);

        // Between-bin values pick the closer side; ties round down.
        act.set_resolution(15);
        assert_eq!(act.resolution(), 10);
        act.set_resolution(16);
        assert_eq!(act.resolution(), 20);
        act.set_resolution(46);
        assert_eq!(act.resolution(), 60);
    }
}
