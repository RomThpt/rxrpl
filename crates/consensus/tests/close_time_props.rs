//! Property tests for adaptive close-time resolution bins and effCloseTime.
//!
//! Exercises rippled-parity invariants in two helpers:
//! - `next_resolution` (close_resolution.rs): output MUST always be one of
//!   the six valid bin widths regardless of `(parent_resolution,
//!   previous_agree, new_ledger_seq)` inputs that themselves come from
//!   the valid bin set.
//! - `eff_close_time` (engine.rs): for any non-zero `close_time`, the
//!   returned value MUST be strictly greater than `prior_close_time`
//!   (monotonicity contract); for `close_time == 0`, MUST passthrough 0.
//!
//! NIGHT-SHIFT-REVIEW: this test reaches `eff_close_time` via the
//! fully-qualified `rxrpl_consensus::engine::eff_close_time` path and
//! `next_resolution` via `rxrpl_consensus::close_resolution::next_resolution`
//! because neither symbol is re-exported from the crate root in
//! `crates/consensus/src/lib.rs` and the T06 whitelist does not include
//! lib.rs. If either symbol gets re-exported later, switch to the
//! shorter `rxrpl_consensus::{eff_close_time, next_resolution}` form.

use proptest::prelude::*;
use rxrpl_consensus::close_resolution::next_resolution;
use rxrpl_consensus::engine::eff_close_time;

const VALID_BINS: [u32; 6] = [10, 20, 30, 60, 90, 120];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// `next_resolution` always returns a value drawn from the rippled
    /// `ledgerPossibleTimeResolutions` set when fed a parent that is
    /// itself in the set. Covers both branches (agree / disagree) and
    /// every non-zero ledger sequence.
    #[test]
    fn next_resolution_yields_valid_bin(
        prev_res in prop::sample::select(VALID_BINS.to_vec()),
        prev_agree in any::<bool>(),
        seq in 1u32..1_000_000,
    ) {
        let next = next_resolution(prev_res, prev_agree, seq);
        prop_assert!(
            VALID_BINS.contains(&next),
            "next_resolution({prev_res}, {prev_agree}, {seq}) = {next} not in valid bins"
        );
    }

    /// For any non-zero `close_time`, `eff_close_time` must clamp the
    /// result to be strictly greater than `prior_close_time`. The input
    /// ranges keep `close + resolution` and `prior + 1` well inside u32
    /// so we exercise the contract, not overflow saturation.
    #[test]
    fn eff_close_time_clamps_above_prior(
        close in 1u32..u32::MAX / 2,
        res in prop::sample::select(VALID_BINS.to_vec()),
        prior in 0u32..u32::MAX / 2,
    ) {
        let eff = eff_close_time(close, res, prior);
        prop_assert!(
            eff > prior,
            "eff_close_time({close}, {res}, {prior}) = {eff} not > prior {prior}"
        );
    }

    /// `close_time == 0` is rippled's "untrusted close time" sentinel
    /// and MUST propagate unchanged regardless of resolution or prior.
    #[test]
    fn eff_close_time_zero_passthrough(
        res in prop::sample::select(VALID_BINS.to_vec()),
        prior in 0u32..u32::MAX / 2,
    ) {
        prop_assert_eq!(eff_close_time(0, res, prior), 0);
    }
}
