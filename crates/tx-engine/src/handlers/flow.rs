//! Multi-path cross-currency Flow engine (rippled `Flow` + `AMMLiquidity`).
//!
//! This module ports rippled's multi-path payment machinery — the part that
//! consumes an AMM pool in *fibonacci-sized synthetic offers* clamped to the
//! competing CLOB quality whenever two or more strands are live. It is the
//! difference between treating the AMM as one full swap (over-delivering) and
//! the byte-exact chunked consumption rippled performs.
//!
//! Scope of what lives here (mirrors go-xrpl's amm_context / amm_liquidity /
//! amm_offer):
//!   * [`AmmContext`]   — per-payment AMM iteration state (rippled `AMMContext`).
//!   * [`AmmLiquidity`] — a pool snapshot with the FROZEN initial balances and
//!     the fib-offer generator (rippled `AMMLiquidity`).
//!   * [`AmmOffer`]     — one synthetic offer with constant-quality ceils
//!     (rippled `AMMOffer`).
//!
//! The pool *mutation* (the actual `swapAssetIn`/`swapAssetOut` + holding moves)
//! reuses [`crate::amm_helpers`] and `offer_create`'s `apply_amm_move` verbatim;
//! only the *sizing* (fib) and the *quality clamp* are new.

#![allow(dead_code)]

use rxrpl_amount::number::{
    MantissaScale, MantissaScaleGuard, Number, RoundModeGuard, RoundingMode,
};
use rxrpl_amount::{
    Amount, IOUAmount, from_rate, get_rate, is_better_quality, within_relative_distance,
};

/// rippled `AMMContext::kMaxIterations` — an AMM may contribute at most 30 fib
/// chunks across the whole multi-path payment.
pub const MAX_AMM_ITERATIONS: u16 = 30;

/// `AMMLiquidity::kFib` (AMMLiquidity.cpp:79-82): the fibonacci multipliers
/// applied to the base output per iteration.
const FIB: [u64; 30] = [
    1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987, 1597, 2584, 4181, 6765, 10946,
    17711, 28657, 46368, 75025, 121393, 196418, 317811, 514229, 832040, 1346269,
];

/// Per-payment AMM state threaded by `&mut` through the whole `flow_multi` call
/// (rippled `AMMContext`, AMMContext.h). Exactly one instance per payment.
#[derive(Clone, Debug)]
pub struct AmmContext {
    multi_path: bool,
    amm_used: bool,
    amm_iters: u16,
}

impl AmmContext {
    /// New context. `multi_path` seeds rippled's `Flow.cpp:106` (= strands.len()
    /// > 1); it is recomputed every pass from the live strand count.
    pub fn new(multi_path: bool) -> Self {
        Self {
            multi_path,
            amm_used: false,
            amm_iters: 0,
        }
    }

    pub fn multi_path(&self) -> bool {
        self.multi_path
    }

    pub fn set_multi_path(&mut self, v: bool) {
        self.multi_path = v;
    }

    /// Mark that an AMM offer was consumed this engine iteration.
    pub fn set_amm_used(&mut self) {
        self.amm_used = true;
    }

    /// Reset the per-iteration `amm_used` flag at the start of each strand trial
    /// (rippled `AMMContext::clear`). A trial that aborts must not burn a fib
    /// step.
    pub fn clear(&mut self) {
        self.amm_used = false;
    }

    /// Commit one engine iteration: advance the fib counter iff an AMM offer was
    /// actually consumed, then reset the flag (rippled `AMMContext::update`).
    pub fn update(&mut self) {
        if self.amm_used {
            self.amm_iters += 1;
        }
        self.amm_used = false;
    }

    pub fn cur_iters(&self) -> u16 {
        self.amm_iters
    }

    pub fn max_iters_reached(&self) -> bool {
        self.amm_iters >= MAX_AMM_ITERATIONS
    }
}

/// Render a `Number` amount onto its asset's grid under the *ambient* rounding
/// mode (rippled `toAmount<T>`): integer drops for XRP, the 16-digit IOU
/// mantissa otherwise. The caller installs the directed `RoundModeGuard`.
fn to_amount_grid(n: &Number, is_xrp: bool) -> Number {
    if is_xrp {
        Number::from_int(n.to_xrp_drops_mode() as i64)
    } else {
        Number::from_iou(&n.to_iou())
    }
}

/// An amount's magnitude as a quality `IOUAmount` (drops counted as an integer),
/// so a rate `in/out` is comparable across XRP and IOU legs — exactly how the
/// book directory encodes quality.
fn quality_iou(n: &Number, is_xrp: bool) -> IOUAmount {
    if is_xrp {
        IOUAmount::from_decimal_string(&n.to_xrp_drops().to_string()).unwrap_or(IOUAmount::ZERO)
    } else {
        n.to_iou()
    }
}

fn number_amount(n: &Number, is_xrp: bool) -> Amount {
    if is_xrp {
        Amount::Xrp(n.to_xrp_drops() as i64)
    } else {
        Amount::Iou(n.to_iou())
    }
}

fn amount_number(amount: Amount) -> Number {
    match amount {
        Amount::Xrp(drops) => Number::from_int(drops),
        Amount::Iou(iou) => Number::from_iou(&iou),
    }
}

/// `a >= b` for two `Number`s.
fn num_ge(a: &Number, b: &Number) -> bool {
    let d = a.sub(b);
    d.is_zero() || !d.negative()
}

/// One AMM pool snapshot for the multi-path strand flow (rippled
/// `AMMLiquidity`). The `initial_*` balances are captured once at strand-build
/// time and held STABLE for the whole payment (the fib seed is computed from
/// them every iteration); the LIVE balances are passed into each call.
#[derive(Clone, Debug)]
pub struct AmmLiquidity {
    pub in_is_xrp: bool,
    pub out_is_xrp: bool,
    pub round_even_quality: bool,
    pub tfee: u16,
    pub initial_pool_in: Number,
    pub initial_pool_out: Number,
}

impl AmmLiquidity {
    /// `AMMLiquidity::generateFibSeqOffer` (AMMLiquidity.cpp:63-100).
    ///
    /// The whole body runs at `MantissaScale::Small` (the swap helpers force it
    /// too; the fib seed math must share the scale so the `toAmount` grid lines
    /// up). The load-bearing asymmetry: `seed_out` is swapped from the FROZEN
    /// `initial_*` balances, while `cur_in` is back-solved against the LIVE
    /// (shrinking) `pool_*` balances.
    pub fn generate_fib_seq_offer(
        &self,
        pool_in: &Number,
        pool_out: &Number,
        ctx: &AmmContext,
    ) -> Option<(Number, Number)> {
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);

        // cur.in = toAmount<In>(kInitialFibSeqPct * initial.in, Upward),
        // kInitialFibSeqPct = 5/20000 = 0.00025.
        let pct = Number::from_int(5).div(&Number::from_int(20_000));
        let seed_in = {
            // The product is evaluated before `toAmount` installs its directed
            // conversion guard, so it keeps the ambient nearest rounding.
            let raw = pct.mul(&self.initial_pool_in);
            let _g = RoundModeGuard::new(RoundingMode::Upward);
            to_amount_grid(&raw, self.in_is_xrp)
        };
        // cur.out = swapAssetIn(initialBalances, cur.in, tfee) (rounds OUT down).
        let seed_out = crate::amm_helpers::swap_asset_in(
            &self.initial_pool_in,
            &self.initial_pool_out,
            &seed_in,
            self.tfee,
            self.out_is_xrp,
        );

        // First AMM pass = the raw base unit.
        if ctx.cur_iters() == 0 {
            if seed_in.is_zero() || seed_out.is_zero() {
                return None;
            }
            return Some((seed_in, seed_out));
        }

        let idx = (ctx.cur_iters() - 1) as usize;
        if idx >= FIB.len() {
            return None;
        }

        // cur.out = toAmount<Out>(seed_out * kFib[idx], Downward).
        let cur_out = {
            // As above, only the asset-grid conversion is directed. Applying
            // Downward to the multiplication itself loses IOU ULPs per chunk.
            let raw = seed_out.mul(&Number::from_int(FIB[idx] as i64));
            let _g = RoundModeGuard::new(RoundingMode::Downward);
            to_amount_grid(&raw, self.out_is_xrp)
        };
        // Overflow: fixAMMOverflowOffer active => nil.
        if num_ge(&cur_out, pool_out) {
            return None;
        }
        // cur.in = swapAssetOut(LIVE balances, cur.out, tfee) (rounds IN up).
        let cur_in = crate::amm_helpers::swap_asset_out(
            pool_in,
            pool_out,
            &cur_out,
            self.tfee,
            self.in_is_xrp,
        )?;
        if cur_in.is_zero() || cur_in.negative() || cur_out.is_zero() {
            return None;
        }
        Some((cur_in, cur_out))
    }

    /// `AMMLiquidity::getOffer` (AMMLiquidity.cpp:154-257).
    ///
    /// Returns the next fib-sized synthetic offer, or `None` when the AMM should
    /// decline this pass: the iteration cap is hit, the pool is frozen, the AMM
    /// spot price does not strictly beat the CLOB tip (or is within `1e-7` of
    /// it), or the generated chunk is itself worse than the CLOB tip.
    ///
    /// `clob_quality` is the competing CLOB tip's packed rate (`in/out`), or
    /// `None` when there is no resting offer at this hop.
    pub fn get_offer(
        &self,
        pool_in: &Number,
        pool_out: &Number,
        clob_quality: Option<u64>,
        ctx: &AmmContext,
    ) -> Option<AmmOffer> {
        if ctx.max_iters_reached() {
            return None;
        }
        if pool_in.is_zero() || pool_out.is_zero() {
            return None;
        }

        // Spot-price-quality gate: the AMM must STRICTLY beat the CLOB and not be
        // within 1e-7 of it.
        let quality_rate = |input: &Number, input_xrp: bool, output: &Number, output_xrp: bool| {
            let input = quality_iou(input, input_xrp);
            let output = quality_iou(output, output_xrp);
            if self.round_even_quality {
                rxrpl_amount::get_rate_round_even(&input, &output)
            } else {
                get_rate(&input, &output)
            }
        };
        let spq = quality_rate(pool_in, self.in_is_xrp, pool_out, self.out_is_xrp).ok()?;
        if let Some(cq) = clob_quality {
            if !is_better_quality(spq, cq) || within_relative_distance(spq, cq) {
                return None;
            }
        }

        let (off_in, off_out, oq) = if ctx.multi_path() {
            // Multiple active strands consume fibonacci-sized synthetic offers.
            let (off_in, off_out) = self.generate_fib_seq_offer(pool_in, pool_out, ctx)?;
            let oq = quality_rate(&off_in, self.in_is_xrp, &off_out, self.out_is_xrp).ok()?;
            if let Some(cq) = clob_quality {
                // Quality{amounts} < clobQuality => decline (chunk worse than CLOB).
                if is_better_quality(cq, oq) {
                    return None;
                }
            }
            (off_in, off_out, oq)
        } else if let Some(cq) = clob_quality {
            // A single strand consumes the AMM until its ending spot price
            // reaches the competing CLOB quality.
            let (off_in, off_out) = crate::amm_helpers::change_spot_price_quality(
                pool_in,
                pool_out,
                cq,
                self.tfee,
                self.in_is_xrp,
                self.out_is_xrp,
            )?;
            let oq = quality_rate(&off_in, self.in_is_xrp, &off_out, self.out_is_xrp).ok()?;
            (off_in, off_out, oq)
        } else {
            // With no competing book, rippled exposes a maximum offer covering
            // 99% of the output reserve. A later limit operation prices the
            // actual demand with the constant-product swap function.
            let pct = Number::from_int(99).div(&Number::from_int(100));
            let off_out = {
                let _round = RoundModeGuard::new(RoundingMode::Downward);
                to_amount_grid(&pool_out.mul(&pct), self.out_is_xrp)
            };
            let off_in = crate::amm_helpers::swap_asset_out(
                pool_in,
                pool_out,
                &off_out,
                self.tfee,
                self.in_is_xrp,
            )?;
            (off_in, off_out, spq)
        };
        if off_in.is_zero() || off_out.is_zero() {
            return None;
        }
        Some(AmmOffer {
            in_num: off_in,
            out_num: off_out,
            bal_in: *pool_in,
            bal_out: *pool_out,
            in_is_xrp: self.in_is_xrp,
            out_is_xrp: self.out_is_xrp,
            multi_path: ctx.multi_path(),
            tfee: self.tfee,
            quality: oq,
        })
    }
}

/// One synthetic AMM offer for a single quality level (rippled `AMMOffer`). In
/// multiPath its `limit_*` are CONSTANT-quality ceils (`ceilOutStrict`/
/// `ceilInStrict`), NOT a fresh swap — this keeps the strand quality order
/// stable across the multi-pass loop.
#[derive(Clone, Debug)]
pub struct AmmOffer {
    pub in_num: Number,
    pub out_num: Number,
    pub bal_in: Number,
    pub bal_out: Number,
    pub in_is_xrp: bool,
    pub out_is_xrp: bool,
    pub multi_path: bool,
    pub tfee: u16,
    /// Packed quality rate (`in/out`) of this chunk.
    pub quality: u64,
}

impl AmmOffer {
    /// `Quality::ceilOutStrict(offerAmount, limit, roundUp)` for the multiPath
    /// AMM offer (AMMOffer.cpp:82 -> Quality.cpp:108): clamp the offer's OUTPUT
    /// to `limit` at this offer's CONSTANT quality. `in = limit * rate` (rounded
    /// per `round_up`), `out = limit`, with `in` clamped to the original.
    pub fn limit_out(&self, limit: &Number, round_up: bool) -> (Number, Number) {
        if !num_gt(&self.out_num, limit) {
            return (self.in_num, self.out_num);
        }
        if !self.multi_path {
            let new_in = crate::amm_helpers::swap_asset_out(
                &self.bal_in,
                &self.bal_out,
                limit,
                self.tfee,
                self.in_is_xrp,
            )
            .unwrap_or(self.in_num);
            return (new_in, *limit);
        }
        let rate = Amount::Iou(from_rate(self.quality).unwrap_or(IOUAmount::ZERO));
        let new_in = Amount::mul_round_strict(
            &number_amount(limit, self.out_is_xrp),
            &rate,
            self.in_is_xrp,
            round_up,
        )
        .map(amount_number)
        .unwrap_or(self.in_num);
        let new_in = if num_gt(&new_in, &self.in_num) {
            self.in_num
        } else {
            new_in
        };
        (new_in, *limit)
    }

    /// `Quality::ceilInStrict(offerAmount, limit, roundUp)` for the multiPath AMM
    /// offer (AMMOffer.cpp:115 -> Quality.cpp:79): clamp the offer's INPUT to
    /// `limit` at constant quality. `out = limit / rate`, `in = limit`, with
    /// `out` clamped to the original.
    pub fn limit_in(&self, limit: &Number, round_up: bool) -> (Number, Number) {
        if !num_gt(&self.in_num, limit) {
            return (self.in_num, self.out_num);
        }
        if !self.multi_path {
            let new_out = crate::amm_helpers::swap_asset_in(
                &self.bal_in,
                &self.bal_out,
                limit,
                self.tfee,
                self.out_is_xrp,
            );
            return (*limit, new_out);
        }
        let rate = Amount::Iou(from_rate(self.quality).unwrap_or(IOUAmount::ZERO));
        let new_out = Amount::div_round(
            &number_amount(limit, self.in_is_xrp),
            &rate,
            self.out_is_xrp,
            round_up,
        )
        .map(amount_number)
        .unwrap_or(Number::ZERO);
        let new_out = if num_gt(&new_out, &self.out_num) {
            self.out_num
        } else {
            new_out
        };
        (*limit, new_out)
    }
}

/// `a > b` for two `Number`s.
fn num_gt(a: &Number, b: &Number) -> bool {
    let d = a.sub(b);
    !d.is_zero() && !d.negative()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_amount::number::Number;

    fn xjoy_xrp_pool() -> AmmLiquidity {
        // XJOY/XRP AMM at ledger 105252960 (parent of the L105252961 target):
        // pool XJOY (in) = 651.8857291500754, pool XRP (out) = 10401722978 drops,
        // trading fee = 994 (0.994%). The payment pays XJOY, receives XRP.
        AmmLiquidity {
            in_is_xrp: false,
            out_is_xrp: true,
            round_even_quality: true,
            tfee: 994,
            initial_pool_in: Number::from_iou(
                &IOUAmount::from_decimal_string("651.8857291500754").unwrap(),
            ),
            initial_pool_out: Number::from_int(10_401_722_978),
        }
    }

    #[test]
    fn initial_multipath_amm_quote_can_miss_passive_bridge_limit() {
        // Ledger 105501035: the bridge's XRP/BTC pool looks competitive at
        // spot, but the initial Fibonacci chunk is just outside the taker's
        // RLUSD/BTC limit. Flow filters it before switching to single path.
        let pool_in = Number::from_int(102_326_553_431);
        let pool_out =
            Number::from_iou(&IOUAmount::from_decimal_string("1.770821155866504").unwrap());
        let amm = AmmLiquidity {
            in_is_xrp: true,
            out_is_xrp: false,
            round_even_quality: true,
            tfee: 19,
            initial_pool_in: pool_in,
            initial_pool_out: pool_out,
        };
        let clob_quality = 6_851_581_433_398_921_448;
        let first_hop = Number::from_iou(&IOUAmount::from_decimal_string("0.00000111259").unwrap());
        let limit = Number::from_iou(&IOUAmount::from_decimal_string("64305.05495616992").unwrap());

        let multi = amm
            .get_offer(
                &pool_in,
                &pool_out,
                Some(clob_quality),
                &AmmContext::new(true),
            )
            .unwrap();
        let multi_rate = Number::from_iou(&from_rate(multi.quality).unwrap());
        assert!(num_gt(&first_hop.mul(&multi_rate), &limit));

        let single = amm
            .get_offer(&pool_in, &pool_out, None, &AmmContext::new(false))
            .unwrap();
        let single_rate = Number::from_iou(&from_rate(single.quality).unwrap());
        assert!(!num_gt(&first_hop.mul(&single_rate), &limit));
    }

    /// Standalone simulation of the multi-pass AMM fib consumption for the
    /// L105252961 repro: budget 17 XJOY, books absorb everything (so only the
    /// XJOY/XRP AMM hop matters). Mirrors `flow_multi` + `execute_strand_pass`'s
    /// AMM hop with the real swap helpers, so we can chase the final-chunk
    /// rounding without the slow play-forward harness. Target XRP = 261_324_008.
    #[test]
    #[ignore]
    fn sim_repro_amm_total() {
        let amm = xjoy_xrp_pool();
        let mut pool_in = amm.initial_pool_in; // XJOY (IOU)
        let mut pool_out = amm.initial_pool_out; // XRP drops
        let send_max = Number::from_int(17);
        let mut ctx = AmmContext::new(true);
        let mut saved_in: Vec<Number> = Vec::new();
        let mut saved_out: Vec<Number> = Vec::new();

        let sum_small = |v: &[Number]| {
            let mut s: Vec<Number> = v.to_vec();
            s.sort_by(|a, b| {
                let d = a.sub(b);
                if d.is_zero() {
                    std::cmp::Ordering::Equal
                } else if d.negative() {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                }
            });
            let mut acc = Number::ZERO;
            for n in &s {
                acc = acc.add(n);
            }
            acc
        };

        for pass in 0..40 {
            let remaining_in = send_max.sub(&sum_small(&saved_in));
            if remaining_in.is_zero() || remaining_in.negative() {
                break;
            }
            ctx.set_multi_path(true);
            let Some(offer) = amm.get_offer(&pool_in, &pool_out, None, &ctx) else {
                break;
            };
            let (mut cin, mut cout) = (offer.in_num, offer.out_num);
            if num_gt(&cin, &remaining_in) {
                let (li, lo) = offer.limit_in(&remaining_in, false);
                cin = li;
                cout = lo;
            }
            if cin.is_zero() || cout.is_zero() {
                break;
            }
            // Mutate pool: XJOY += cin (IOU-grid rounded like set_iou_holding);
            // XRP -= cout (exact drops).
            let new_in = pool_in.add(&cin);
            pool_in = Number::from_iou(&new_in.to_iou());
            pool_out = pool_out.sub(&cout);
            eprintln!(
                "pass {pass} iter={} cin={} cout_drops={} pool_xrp={}",
                ctx.cur_iters(),
                cin.to_iou().to_decimal_string(),
                cout.to_xrp_drops(),
                pool_out.to_xrp_drops(),
            );
            saved_in.push(cin);
            saved_out.push(cout);
            ctx.set_amm_used();
            ctx.update();
        }
        let total_out = sum_small(&saved_out);
        eprintln!(
            "TOTAL XRP = {} (target 261324008, ours-was 261406413)",
            total_out.to_xrp_drops()
        );
        eprintln!(
            "TOTAL XJOY in = {}",
            sum_small(&saved_in).to_iou().to_decimal_string()
        );
    }

    #[test]
    fn amm_context_counter_advances_only_when_used() {
        let mut ctx = AmmContext::new(true);
        assert_eq!(ctx.cur_iters(), 0);
        assert!(ctx.multi_path());

        // An iteration that does NOT consume the AMM does not advance.
        ctx.clear();
        ctx.update();
        assert_eq!(ctx.cur_iters(), 0);

        // An iteration that consumes the AMM advances by one.
        ctx.clear();
        ctx.set_amm_used();
        ctx.update();
        assert_eq!(ctx.cur_iters(), 1);

        // A trial that sets used then is cleared (aborted) does not advance.
        ctx.clear();
        ctx.set_amm_used();
        ctx.clear();
        ctx.update();
        assert_eq!(ctx.cur_iters(), 1);

        // Cap at 30.
        for _ in 0..40 {
            ctx.clear();
            ctx.set_amm_used();
            ctx.update();
        }
        assert!(ctx.max_iters_reached());
        // 1 from the earlier consume + 40 more = 41 (the counter is not capped,
        // only `max_iters_reached` reports the threshold).
        assert_eq!(ctx.cur_iters(), 41);
    }

    #[test]
    fn fib_seed_is_quarter_basis_point_of_initial_in() {
        let amm = xjoy_xrp_pool();
        let ctx = AmmContext::new(true);
        let (in0, out0) = amm
            .generate_fib_seq_offer(&amm.initial_pool_in, &amm.initial_pool_out, &ctx)
            .expect("seed offer");
        // seed_in = ceil(0.00025 * 651.8857291500754) = 0.162971432287518... on
        // the IOU grid (rounded up).
        let in0_s = in0.to_iou().to_decimal_string();
        assert!(in0_s.starts_with("0.16297143228751"), "seed_in = {in0_s}");
        // seed_out is a positive integer drop count well under the pool.
        let drops = out0.to_xrp_drops();
        assert!(
            drops > 0 && drops < 10_401_722_978,
            "seed_out drops = {drops}"
        );
    }

    #[test]
    fn fib_output_scales_by_fibonacci() {
        // The base output (iter 0) and the iter-k outputs scale ~ fib[k-1].
        let amm = xjoy_xrp_pool();
        let mut ctx = AmmContext::new(true);

        let (_in0, out0) = amm
            .generate_fib_seq_offer(&amm.initial_pool_in, &amm.initial_pool_out, &ctx)
            .expect("base");
        let base = out0.to_xrp_drops() as f64;

        // iter 1 => 1x base, iter 2 => 2x, iter 3 => 3x, iter 4 => 5x ...
        let expect = [1u64, 2, 3, 5, 8];
        for (i, mult) in expect.iter().enumerate() {
            ctx.set_amm_used();
            ctx.update(); // advance to iter i+1
            assert_eq!(ctx.cur_iters() as usize, i + 1);
            let (cin, cout) = amm
                .generate_fib_seq_offer(&amm.initial_pool_in, &amm.initial_pool_out, &ctx)
                .expect("fib offer");
            let got = cout.to_xrp_drops() as f64;
            let want = base * (*mult as f64);
            // Within 0.5% (Number directed rounding + the back-solve drift).
            assert!(
                (got - want).abs() / want < 0.005,
                "iter {} out={} want~={} (mult {})",
                i + 1,
                got,
                want,
                mult
            );
            assert!(!cin.is_zero() && !cin.negative());
        }
    }

    #[test]
    fn get_offer_declines_when_spot_price_within_clob() {
        let amm = xjoy_xrp_pool();
        let ctx = AmmContext::new(true);
        // Spot price quality = in/out = 651.88.../10401722978.
        let spq = get_rate(
            &amm.initial_pool_in.to_iou(),
            &IOUAmount::from_decimal_string("10401722978").unwrap(),
        )
        .unwrap();
        // A CLOB tip exactly equal to the spot price => decline (not strictly
        // better, and within distance).
        assert!(
            amm.get_offer(&amm.initial_pool_in, &amm.initial_pool_out, Some(spq), &ctx)
                .is_none()
        );
        // A CLOB tip far WORSE than spot (higher rate) => the AMM produces a
        // chunk (it strictly beats the CLOB).
        let worse = get_rate(
            &IOUAmount::from_decimal_string("1.0").unwrap(),
            &IOUAmount::from_decimal_string("1.0").unwrap(),
        )
        .unwrap(); // rate 1.0, far worse than ~6e-8 spot
        assert!(
            amm.get_offer(
                &amm.initial_pool_in,
                &amm.initial_pool_out,
                Some(worse),
                &ctx
            )
            .is_some()
        );
    }

    #[test]
    fn get_offer_none_when_iters_maxed() {
        let amm = xjoy_xrp_pool();
        let mut ctx = AmmContext::new(true);
        for _ in 0..MAX_AMM_ITERATIONS {
            ctx.set_amm_used();
            ctx.update();
        }
        assert!(ctx.max_iters_reached());
        assert!(
            amm.get_offer(&amm.initial_pool_in, &amm.initial_pool_out, None, &ctx)
                .is_none()
        );
    }

    #[test]
    fn ammoffer_limit_out_constant_quality() {
        let amm = xjoy_xrp_pool();
        let ctx = AmmContext::new(true);
        let offer = amm
            .get_offer(&amm.initial_pool_in, &amm.initial_pool_out, None, &ctx)
            .expect("offer");
        // Limit the output to half the chunk: in scales ~proportionally, out=limit.
        let half = Number::from_int((offer.out_num.to_xrp_drops() / 2) as i64);
        let (lin, lout) = offer.limit_out(&half, true);
        assert_eq!(lout.to_xrp_drops(), half.to_xrp_drops());
        assert!(num_gt(&offer.in_num, &lin) || offer.in_num == lin);
        // Quality is preserved (constant-quality ceil): in/out ~ original.
        let q = get_rate(&lin.to_iou(), &quality_iou(&lout, true)).unwrap();
        let approx = (q as f64 - offer.quality as f64).abs() / offer.quality as f64;
        assert!(approx < 0.01, "quality drift {approx}");
    }

    #[test]
    fn single_path_amm_limit_in_reprices_the_swap() {
        let amm = xjoy_xrp_pool();
        let ctx = AmmContext::new(false);
        let offer = amm
            .get_offer(&amm.initial_pool_in, &amm.initial_pool_out, None, &ctx)
            .expect("maximum single-path offer");
        let limit = Number::from_iou(&IOUAmount::from_decimal_string("1").unwrap());
        let expected = crate::amm_helpers::swap_asset_in(
            &amm.initial_pool_in,
            &amm.initial_pool_out,
            &limit,
            amm.tfee,
            amm.out_is_xrp,
        );

        let (priced_in, priced_out) = offer.limit_in(&limit, false);

        assert_eq!(priced_in, limit);
        assert_eq!(priced_out, expected);
    }

    #[test]
    fn single_path_amm_limit_out_reprices_the_swap() {
        let amm = xjoy_xrp_pool();
        let ctx = AmmContext::new(false);
        let offer = amm
            .get_offer(&amm.initial_pool_in, &amm.initial_pool_out, None, &ctx)
            .expect("maximum single-path offer");
        let limit = Number::from_int(1_000_000);
        let expected = crate::amm_helpers::swap_asset_out(
            &amm.initial_pool_in,
            &amm.initial_pool_out,
            &limit,
            amm.tfee,
            amm.in_is_xrp,
        )
        .expect("priced output");

        let (priced_in, priced_out) = offer.limit_out(&limit, true);

        assert_eq!(priced_in, expected);
        assert_eq!(priced_out, limit);
    }

    #[test]
    fn modern_amm_quality_rounding_pins_sendmax_limited_chunk() {
        // Ledger 105500174, final AmeriCoin/EuroCoin fib chunk. The synthetic
        // offer quality is constructed after fixUniversalNumber, so getRate's
        // quotient must reduce half-to-even before ceilInStrict prices the
        // remaining SendMax.
        let in_num =
            Number::from_iou(&IOUAmount::from_decimal_string("567398397.3026964").unwrap());
        let out_num = Number::from_iou(&IOUAmount::from_decimal_string("676942644.39939").unwrap());
        let modern_quality =
            rxrpl_amount::get_rate_round_even(&in_num.to_iou(), &out_num.to_iou()).unwrap();
        let offer = AmmOffer {
            in_num,
            out_num,
            bal_in: Number::ZERO,
            bal_out: Number::ZERO,
            in_is_xrp: false,
            out_is_xrp: false,
            multi_path: true,
            tfee: 1000,
            quality: modern_quality,
        };
        let limit = Number::from_iou(&IOUAmount::from_decimal_string("225309064.044909").unwrap());
        let (priced_in, priced_out) = offer.limit_in(&limit, false);
        assert_eq!(priced_in.to_iou().to_decimal_string(), "225309064.044909");
        assert_eq!(priced_out.to_iou().to_decimal_string(), "268808150.2287799");
    }

    #[test]
    fn modern_amm_quality_rounding_prices_path_chunk() {
        // The AmeriCoin/XRP hop selected at AMM iteration 10 in ledger
        // 105500174. Its generated quality differs by one packed-rate ULP after
        // fixUniversalNumber; ceilOutStrict must use the modern value.
        let _scale = MantissaScaleGuard::new(MantissaScale::Small);
        let initial_in =
            Number::from_iou(&IOUAmount::from_decimal_string("45338846767.35569").unwrap());
        let initial_out = Number::from_int(42_720_824);
        let amm = AmmLiquidity {
            in_is_xrp: false,
            out_is_xrp: true,
            round_even_quality: true,
            tfee: 1000,
            initial_pool_in: initial_in,
            initial_pool_out: initial_out,
        };
        let pool_in = initial_in
            .add(&Number::from_iou(
                &IOUAmount::from_decimal_string("43088831.32872305").unwrap(),
            ))
            .add(&Number::from_iou(
                &IOUAmount::from_decimal_string("183223338.4376844").unwrap(),
            ));
        let pool_out = Number::from_int(42_720_824 - 40_145 - 169_707);
        let mut ctx = AmmContext::new(true);
        for _ in 0..10 {
            ctx.set_amm_used();
            ctx.update();
        }
        let offer = amm
            .get_offer(&pool_in, &pool_out, None, &ctx)
            .expect("iteration-10 offer");
        let legacy_quality = get_rate(
            &quality_iou(&offer.in_num, false),
            &quality_iou(&offer.out_num, true),
        )
        .unwrap();
        assert_ne!(offer.quality, legacy_quality);

        let (priced_in, priced_out) = offer.limit_out(&Number::from_int(739_489), true);
        assert_eq!(priced_out.to_xrp_drops(), 739_489);
        assert_eq!(priced_in.to_iou().to_decimal_string(), "818741605.6922346");
    }
}
