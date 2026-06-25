//! Port of rippled's `Number` (Large mantissa scale) for AMM arithmetic.
//!
//! AMM pool math (`AMMHelpers.cpp`) is computed in rippled's `Number` type, not
//! in legacy STAmount arithmetic. To reproduce pool/LPToken amounts byte-for-byte
//! this mirrors `Number` at the "Large" scale (18-digit mantissa) with the same
//! guard-digit rounding (half-to-even by default, plus directed modes) and the
//! same `root2` square root. See `reference_amm_number_port` for the full spec.

use std::cell::Cell;

const MIN_MANTISSA: u64 = 1_000_000_000_000_000_000; // 10^18
const MAX_MANTISSA: u64 = 9_999_999_999_999_999_999; // 10^19 - 1
const MAX_REP: u64 = 9_223_372_036_854_775_807; // i64::MAX
const MIN_EXPONENT: i32 = -32768;
const MAX_EXPONENT: i32 = 32768;
const MANTISSA_LOG: i32 = 18;
const ZERO_EXPONENT: i32 = i32::MIN;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundingMode {
    ToNearest,
    TowardsZero,
    Downward,
    Upward,
}

thread_local! {
    static MODE: Cell<RoundingMode> = const { Cell::new(RoundingMode::ToNearest) };
}

fn getround() -> RoundingMode {
    MODE.with(|m| m.get())
}

fn setround(mode: RoundingMode) -> RoundingMode {
    MODE.with(|m| m.replace(mode))
}

/// RAII guard restoring the previous rounding mode on drop.
pub struct RoundModeGuard(RoundingMode);

impl RoundModeGuard {
    pub fn new(mode: RoundingMode) -> Self {
        RoundModeGuard(setround(mode))
    }
}

impl Drop for RoundModeGuard {
    fn drop(&mut self) {
        setround(self.0);
    }
}

/// 16 decimal guard digits packed as hex nibbles, plus sticky and sign bits.
#[derive(Default)]
struct Guard {
    digits: u64,
    xbit: bool,
    sbit: bool,
}

impl Guard {
    fn set_negative(&mut self) {
        self.sbit = true;
    }
    fn set_dropped(&mut self) {
        self.xbit = true;
    }

    fn do_push(&mut self, d: u32) {
        self.xbit = self.xbit || (self.digits & 0xF) != 0;
        self.digits >>= 4;
        self.digits |= ((d as u64) & 0xF) << 60;
    }

    fn pop(&mut self) -> u32 {
        let d = (self.digits & 0xF000_0000_0000_0000) >> 60;
        self.digits <<= 4;
        d as u32
    }

    fn drop_digit_u128(&mut self, mantissa: &mut u128, exponent: &mut i32) {
        self.do_push((*mantissa % 10) as u32);
        *mantissa /= 10;
        *exponent += 1;
    }

    /// -1 below half, 0 exactly half, +1 above half (relative to rounding mode).
    fn round(&self) -> i32 {
        match getround() {
            RoundingMode::TowardsZero => -1,
            RoundingMode::Downward => {
                if self.sbit && (self.digits > 0 || self.xbit) {
                    1
                } else {
                    -1
                }
            }
            RoundingMode::Upward => {
                if self.sbit {
                    -1
                } else if self.digits > 0 || self.xbit {
                    1
                } else {
                    -1
                }
            }
            RoundingMode::ToNearest => {
                if self.digits > 0x5000_0000_0000_0000 {
                    1
                } else if self.digits < 0x5000_0000_0000_0000 {
                    -1
                } else if self.xbit {
                    1
                } else {
                    0
                }
            }
        }
    }

    fn bring_into_range(&self, _negative: &mut bool, mantissa: &mut u128, exponent: &mut i32) {
        if *mantissa < MIN_MANTISSA as u128 {
            *mantissa *= 10;
            *exponent -= 1;
        }
        if *exponent < MIN_EXPONENT {
            *mantissa = 0;
            *exponent = ZERO_EXPONENT;
            *_negative = false;
        }
    }

    fn do_round_up(&mut self, negative: &mut bool, mantissa: &mut u128, exponent: &mut i32) {
        let r = self.round();
        if r == 1 || (r == 0 && (*mantissa & 1) == 1) {
            let safe = *mantissa < MAX_MANTISSA as u128 && *mantissa < MAX_REP as u128;
            if safe {
                *mantissa += 1;
            } else {
                // Incrementing would overflow the range; drop a digit and
                // re-evaluate rounding with the updated guard (rippled recurses
                // here, at most once).
                self.drop_digit_u128(mantissa, exponent);
                self.do_round_up(negative, mantissa, exponent);
                return;
            }
        }
        self.bring_into_range(negative, mantissa, exponent);
    }

    fn do_round_down(&mut self, negative: &mut bool, mantissa: &mut u128, exponent: &mut i32) {
        let r = self.round();
        if r == 1 || (r == 0 && (*mantissa & 1) == 1) {
            *mantissa -= 1;
            if *mantissa < MIN_MANTISSA as u128 {
                *mantissa *= 10;
                *exponent -= 1;
            }
        }
        self.bring_into_range(negative, mantissa, exponent);
    }
}

fn do_normalize(negative: &mut bool, mantissa: &mut u128, exponent: &mut i32, dropped: bool) {
    if *mantissa == 0 {
        *mantissa = 0;
        *exponent = ZERO_EXPONENT;
        *negative = false;
        return;
    }
    let mut m = *mantissa;
    while m < MIN_MANTISSA as u128 && *exponent > MIN_EXPONENT {
        m *= 10;
        *exponent -= 1;
    }
    let mut g = Guard::default();
    if *negative {
        g.set_negative();
    }
    if dropped {
        g.set_dropped();
    }
    while m > MAX_MANTISSA as u128 {
        g.drop_digit_u128(&mut m, exponent);
    }
    if *exponent < MIN_EXPONENT || m < MIN_MANTISSA as u128 {
        *mantissa = 0;
        *exponent = ZERO_EXPONENT;
        *negative = false;
        return;
    }
    if m > MAX_REP as u128 {
        g.drop_digit_u128(&mut m, exponent);
    }
    *mantissa = m;
    g.do_round_up(negative, mantissa, exponent);
}

#[derive(Clone, Copy, Debug)]
pub struct Number {
    negative: bool,
    mantissa: u64,
    exponent: i32,
}

impl Number {
    pub const ZERO: Number = Number {
        negative: false,
        mantissa: 0,
        exponent: ZERO_EXPONENT,
    };

    /// Construct from raw parts and normalize to the Large-scale canonical form.
    pub fn new(negative: bool, mantissa: u64, exponent: i32) -> Number {
        let mut neg = negative;
        let mut m = mantissa as u128;
        let mut e = exponent;
        do_normalize(&mut neg, &mut m, &mut e, false);
        Number {
            negative: neg,
            mantissa: m as u64,
            exponent: e,
        }
    }

    pub fn from_int(v: i64) -> Number {
        if v == 0 {
            return Number::ZERO;
        }
        Number::new(v < 0, v.unsigned_abs(), 0)
    }

    pub fn one() -> Number {
        Number {
            negative: false,
            mantissa: MIN_MANTISSA,
            exponent: -MANTISSA_LOG,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    pub fn mantissa(&self) -> u64 {
        self.mantissa
    }
    pub fn exponent(&self) -> i32 {
        self.exponent
    }
    pub fn negative(&self) -> bool {
        self.negative
    }

    pub fn negate(mut self) -> Number {
        if self.mantissa != 0 {
            self.negative = !self.negative;
        }
        self
    }

    fn shift_exponent(&self, delta: i32) -> Number {
        let new_exp = self.exponent + delta;
        if new_exp >= MAX_EXPONENT {
            // Overflow: clamp by panicking would be wrong; callers in root2 stay in range.
            return Number {
                negative: self.negative,
                mantissa: self.mantissa,
                exponent: new_exp,
            };
        }
        if new_exp < MIN_EXPONENT {
            return Number::ZERO;
        }
        Number {
            negative: self.negative,
            mantissa: self.mantissa,
            exponent: new_exp,
        }
    }

    pub fn mul(&self, y: &Number) -> Number {
        if self.is_zero() || y.is_zero() {
            return Number::ZERO;
        }
        let mut zm = (self.mantissa as u128) * (y.mantissa as u128);
        let mut ze = self.exponent + y.exponent;
        let mut zn = self.negative != y.negative;
        let mut g = Guard::default();
        if zn {
            g.set_negative();
        }
        while zm > MAX_MANTISSA as u128 || zm > MAX_REP as u128 {
            g.drop_digit_u128(&mut zm, &mut ze);
        }
        g.do_round_up(&mut zn, &mut zm, &mut ze);
        let mut neg = zn;
        do_normalize(&mut neg, &mut zm, &mut ze, false);
        Number {
            negative: neg,
            mantissa: zm as u64,
            exponent: ze,
        }
    }

    pub fn div(&self, y: &Number) -> Number {
        assert!(!y.is_zero(), "Number divide by zero");
        if self.is_zero() {
            return Number::ZERO;
        }
        let nm = self.mantissa as u128;
        let ne = self.exponent;
        let dm = y.mantissa as u128;
        let de = y.exponent;
        let zp = self.negative != y.negative;

        const FACTOR_EXP: i32 = 17;
        let f: u128 = 10u128.pow(FACTOR_EXP as u32);
        let numerator = nm * f;
        let mut zm = numerator / dm;
        let mut ze = ne - de - FACTOR_EXP;
        let mut dropped = false;

        // Stage 2 (Large scale always): correction factor 10^5.
        const CORRECTION_EXP: i32 = 5;
        let correction_factor: u128 = 10u128.pow(CORRECTION_EXP as u32);
        let remainder = numerator % dm;
        if remainder != 0 {
            let partial_numerator = remainder * correction_factor;
            let correction = partial_numerator / dm;
            if correction != 0 {
                zm *= correction_factor;
                ze -= CORRECTION_EXP;
                zm += correction;
            }
            // Stage 3: cusp rounding fix is enabled for Large.
            dropped = partial_numerator % dm != 0;
        }

        let mut neg = zp;
        do_normalize(&mut neg, &mut zm, &mut ze, dropped);
        Number {
            negative: neg,
            mantissa: zm as u64,
            exponent: ze,
        }
    }

    pub fn add(&self, y: &Number) -> Number {
        if y.is_zero() {
            return *self;
        }
        if self.is_zero() {
            return *y;
        }
        if *self == y.negate() {
            return Number::ZERO;
        }

        let mut xn = self.negative;
        let mut xm = self.mantissa as u128;
        let mut xe = self.exponent;
        let yn = y.negative;
        let mut ym = y.mantissa as u128;
        let mut ye = y.exponent;
        let mut g = Guard::default();

        if xe < ye {
            if xn {
                g.set_negative();
            }
            while xe < ye {
                g.drop_digit_u128(&mut xm, &mut xe);
            }
        } else if xe > ye {
            if yn {
                g.set_negative();
            }
            while xe > ye {
                g.drop_digit_u128(&mut ym, &mut ye);
            }
        }

        if xn == yn {
            xm += ym;
            if xm > MAX_MANTISSA as u128 || xm > MAX_REP as u128 {
                g.drop_digit_u128(&mut xm, &mut xe);
            }
            g.do_round_up(&mut xn, &mut xm, &mut xe);
        } else {
            if xm > ym {
                xm -= ym;
            } else {
                xm = ym - xm;
                xe = ye;
                xn = yn;
            }
            while xm < MIN_MANTISSA as u128 && xm * 10 <= MAX_REP as u128 {
                xm *= 10;
                xm -= g.pop() as u128;
                xe -= 1;
            }
            g.do_round_down(&mut xn, &mut xm, &mut xe);
        }

        let mut neg = xn;
        do_normalize(&mut neg, &mut xm, &mut xe, false);
        Number {
            negative: neg,
            mantissa: xm as u64,
            exponent: xe,
        }
    }

    pub fn sub(&self, y: &Number) -> Number {
        self.add(&y.negate())
    }

    /// Convert a 16-digit `IOUAmount` to a Number (exact: scales the mantissa
    /// up into the 18-digit Large range).
    pub fn from_iou(iou: &crate::iou::IOUAmount) -> Number {
        if iou.is_zero() {
            return Number::ZERO;
        }
        Number::new(iou.sign_bit(), iou.mantissa(), iou.exponent())
    }

    /// Reduce to a canonical 16-digit `IOUAmount` (STAmount IOU precision),
    /// honouring the active rounding mode — matching rippled's `toSTAmount`,
    /// whose 18→16 reduction goes through `Number` under the same mode guard.
    pub fn to_iou(&self) -> crate::iou::IOUAmount {
        use crate::iou::IOUAmount;
        if self.is_zero() {
            return IOUAmount::ZERO;
        }
        const IOU_MAX: u128 = 9_999_999_999_999_999; // 10^16 - 1
        let mut m = self.mantissa as u128;
        let mut e = self.exponent;
        let mut drop = 0u32;
        let mut probe = m;
        while probe > IOU_MAX {
            probe /= 10;
            drop += 1;
        }
        if drop > 0 {
            let div = 10u128.pow(drop);
            let q = m / div;
            let r = m % div;
            let half = div / 2;
            let round_up = match getround() {
                RoundingMode::ToNearest => r > half || (r == half && (q & 1 == 1)),
                RoundingMode::TowardsZero => false,
                RoundingMode::Downward => self.negative && r != 0,
                RoundingMode::Upward => !self.negative && r != 0,
            };
            m = if round_up { q + 1 } else { q };
            e += drop as i32;
            while m > IOU_MAX {
                m /= 10;
                e += 1;
            }
        }
        IOUAmount::from_parts(m as u64, e, self.negative).unwrap_or(IOUAmount::ZERO)
    }

    /// Convert to an `i64` matching rippled's `Number::operator rep()`:
    /// round to nearest, ties to even, using the active rounding mode for the
    /// dropped fractional digits.
    pub fn to_i64(&self) -> i64 {
        if self.is_zero() {
            return 0;
        }
        let mut drops = self.mantissa as i128;
        let mut offset = self.exponent;
        let mut g = Guard::default();
        if self.negative {
            g.set_negative();
            drops = -drops;
        }
        while offset < 0 {
            g.do_push((drops % 10).unsigned_abs() as u32);
            drops /= 10;
            offset += 1;
        }
        while offset > 0 {
            drops = drops.saturating_mul(10);
            offset -= 1;
        }
        let r = g.round();
        if r == 1 || (r == 0 && (drops & 1) == 1) {
            drops += 1;
        }
        if self.negative {
            drops = -drops;
        }
        drops.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    /// Convert to integer XRP drops, truncating toward zero (floor for the
    /// non-negative values AMM payouts produce under downward rounding).
    pub fn to_xrp_drops(&self) -> u64 {
        if self.is_zero() {
            return 0;
        }
        let m = self.mantissa as u128;
        let v = if self.exponent >= 0 {
            m.saturating_mul(10u128.pow(self.exponent as u32))
        } else {
            m / 10u128.pow((-self.exponent) as u32)
        };
        v.min(u64::MAX as u128) as u64
    }

    /// Convert to integer XRP drops honouring the active rounding mode, matching
    /// rippled's `toSTAmount` for an XRP asset (`operator rep()` rounds the
    /// fractional part under the thread-local mode).
    pub fn to_xrp_drops_mode(&self) -> u64 {
        if self.is_zero() {
            return 0;
        }
        let m = self.mantissa as u128;
        if self.exponent >= 0 {
            return m
                .saturating_mul(10u128.pow(self.exponent as u32))
                .min(u64::MAX as u128) as u64;
        }
        let div = 10u128.pow((-self.exponent) as u32);
        let q = m / div;
        let r = m % div;
        let half = div / 2;
        let round_up = match getround() {
            RoundingMode::ToNearest => r > half || (r == half && (q & 1 == 1)),
            RoundingMode::TowardsZero => false,
            RoundingMode::Downward => self.negative && r != 0,
            RoundingMode::Upward => !self.negative && r != 0,
        };
        let v = if round_up { q + 1 } else { q };
        v.min(u64::MAX as u128) as u64
    }

    /// Decimal-string form (full 18-digit mantissa), for tests/debugging.
    pub fn to_decimal_string(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let sign = if self.negative { "-" } else { "" };
        let digits = self.mantissa.to_string();
        let e = self.exponent;
        if e >= 0 {
            return format!("{sign}{digits}{}", "0".repeat(e as usize));
        }
        let frac = (-e) as usize;
        if frac >= digits.len() {
            let zeros = "0".repeat(frac - digits.len());
            format!("{sign}0.{zeros}{}", digits.trim_end_matches('0'))
        } else {
            let point = digits.len() - frac;
            let (int_part, frac_part) = digits.split_at(point);
            let frac_part = frac_part.trim_end_matches('0');
            if frac_part.is_empty() {
                format!("{sign}{int_part}")
            } else {
                format!("{sign}{int_part}.{frac_part}")
            }
        }
    }
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        self.negative == other.negative
            && self.mantissa == other.mantissa
            && self.exponent == other.exponent
    }
}
impl Eq for Number {}

/// `f^n` by square-and-multiply (`log2(n)` multiplications), matching rippled
/// `power(Number f, unsigned n)`.
pub fn power(f: &Number, n: u32) -> Number {
    if n == 0 {
        return Number::one();
    }
    if n == 1 {
        return *f;
    }
    let mut r = power(f, n / 2);
    r = r.mul(&r);
    if n % 2 != 0 {
        r = r.mul(f);
    }
    r
}

/// Square root via Newton-Raphson with a quadratic seed, matching rippled `root2`.
pub fn root2(mut f: Number) -> Number {
    let one = Number::one();
    if f == one {
        return f;
    }
    assert!(!f.negative, "root2 of negative");
    if f.is_zero() {
        return f;
    }

    let mut e = f.exponent + MANTISSA_LOG + 1;
    if e % 2 != 0 {
        e += 1;
    }
    f = f.shift_exponent(-e);

    let a0 = Number::from_int(18);
    let a1 = Number::from_int(144);
    let a2 = Number::from_int(-60);
    let dd = Number::from_int(105);
    // r = ((a2*f + a1)*f + a0) / D
    let mut r = a2.mul(&f).add(&a1).mul(&f).add(&a0).div(&dd);

    let two = Number::from_int(2);
    let mut rm1 = Number::ZERO;
    let mut rm2;
    loop {
        rm2 = rm1;
        rm1 = r;
        r = r.add(&f.div(&r)).div(&two);
        if r == rm1 || r == rm2 {
            break;
        }
    }
    r.shift_exponent(e / 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_simple() {
        let a = Number::from_int(2);
        let b = Number::from_int(3);
        assert_eq!(a.mul(&b), Number::from_int(6));
    }

    #[test]
    fn div_half() {
        let a = Number::from_int(1);
        let b = Number::from_int(2);
        let r = a.div(&b);
        // 0.5 = 5 * 10^-1, normalized to 5*10^17 mantissa, exp -18.
        assert_eq!(r.mul(&Number::from_int(2)), Number::from_int(1));
    }

    #[test]
    fn sqrt_four() {
        assert_eq!(root2(Number::from_int(4)), Number::from_int(2));
    }

    #[test]
    fn sqrt_nine() {
        assert_eq!(root2(Number::from_int(9)), Number::from_int(3));
    }

    #[test]
    fn add_sub_roundtrip() {
        let a = Number::from_int(7);
        let b = Number::from_int(5);
        assert_eq!(a.add(&b), Number::from_int(12));
        assert_eq!(a.sub(&b), Number::from_int(2));
    }

    // AMM single-asset deposit (oracle 105093427): deposit 10000 drops XRP into
    // the XRP/QZilla pool. Reproduces rippled's `lpTokensOut`.
    #[test]
    fn amm_single_deposit_tokens() {
        let tfee = Number::from_int(214);
        let f1 = Number::from_int(1).sub(&tfee.div(&Number::from_int(100_000)));
        let f2 = Number::from_int(1)
            .sub(&tfee.div(&Number::from_int(200_000)))
            .div(&f1);
        let balance = Number::from_int(2_151_064_661); // pool XRP (drops)
        let deposit = Number::from_int(10_000);
        // lptAMMBalance T = 19958158559.84553
        let t = Number::new(false, 1_995_815_855_984_553, -5);
        let r = deposit.div(&balance);
        let c = root2(f2.mul(&f2).add(&r.div(&f1))).sub(&f2);
        let frac = r.sub(&c).div(&Number::from_int(1).add(&c));
        let tokens = {
            let _g = RoundModeGuard::new(RoundingMode::Downward);
            t.mul(&frac)
        };
        // Infinite-precision tokens = 46341.6038725638…; Number reproduces it to
        // ~11 significant digits (full byte-exactness is checked once the AMM
        // handler is wired and run against the on-ledger oracle).
        let s = tokens.to_decimal_string();
        assert!(
            s.starts_with("46341.6038725"),
            "tokens = {s} (expected ~46341.6038725…)"
        );
        // T + tokens, then displayed, must lead with the on-ledger LPTokenBalance.
        let new_t = t.add(&tokens).to_decimal_string();
        assert!(
            new_t.starts_with("19958204901.4494"),
            "new LPTokenBalance = {new_t} (expected 19958204901.4494…)"
        );
    }

    #[test]
    fn root2_of_root2() {
        // sqrt(2) ~ 1.4142135623730951
        let s = root2(Number::from_int(2)).to_decimal_string();
        assert!(s.starts_with("1.41421356237309"), "sqrt(2) = {s}");
    }
}
