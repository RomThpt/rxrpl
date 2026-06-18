//! Unified XRP-or-IOU amount with rippled-exact arithmetic.
//!
//! rippled's `STAmount` holds either native XRP (drops) or an IOU
//! (mantissa/exponent), and its `multiply`/`divide`/`mulRound`/`divRound` free
//! functions take operands of either kind and produce a result whose *issue*
//! (native or not) the caller chooses. The offer-crossing engine relies on this
//! native↔IOU mixing, so [`Amount`] generalises [`IOUAmount`] to both.
//!
//! The math mirrors `STAmount.cpp` byte-for-byte for the pre-`Number`-switchover
//! (2013-era) regime used to replay historical ledgers: non-rounding `multiply`
//! uses `(m1*m2)/10^14 + 7`, `divide` uses `(m1*10^17)/m2 + 5`, and the
//! directional `mulRound`/`divRound` apply the legacy `canonicalizeRound`
//! (the `loops>=2 ? 9 : 10` native-drops quirk). Cross-checked against
//! go-xrpl's `MulRoundNative`/`CanonicalizeDrops` port.

use crate::error::AmountError;
use crate::iou::{IOUAmount, MAX_MANTISSA, MIN_MANTISSA};

/// 10^14, the multiply divisor.
const TEN_TO_14: u128 = 100_000_000_000_000;
/// 10^17, the divide multiplier.
const TEN_TO_17: u128 = 100_000_000_000_000_000;
/// Largest representable XRP, in drops (10^17 = 100e9 XRP × 10^6).
const MAX_NATIVE_DROPS: u64 = 100_000_000_000_000_000;

/// An XRP (drops) or IOU amount, mirroring rippled's `STAmount`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    /// Native XRP, in signed drops.
    Xrp(i64),
    /// An issued (IOU) amount.
    Iou(IOUAmount),
}

impl Amount {
    /// True for native XRP.
    pub fn is_native(&self) -> bool {
        matches!(self, Amount::Xrp(_))
    }

    /// True when the amount is exactly zero.
    pub fn is_zero(&self) -> bool {
        match self {
            Amount::Xrp(d) => *d == 0,
            Amount::Iou(a) => a.is_zero(),
        }
    }

    /// True for a strictly negative amount.
    pub fn is_negative(&self) -> bool {
        match self {
            Amount::Xrp(d) => *d < 0,
            Amount::Iou(a) => a.is_negative(),
        }
    }

    /// The signed drop count, or `None` for an IOU.
    pub fn drops(&self) -> Option<i64> {
        match self {
            Amount::Xrp(d) => Some(*d),
            Amount::Iou(_) => None,
        }
    }

    /// The IOU value, or `None` for native XRP.
    pub fn iou(&self) -> Option<IOUAmount> {
        match self {
            Amount::Iou(a) => Some(*a),
            Amount::Xrp(_) => None,
        }
    }

    /// `(|mantissa|, exponent, negative)`, scaling a native value's drops into
    /// the IOU mantissa range — the common entry to rippled's mul/div, which
    /// raises each operand's mantissa to at least `cMinValue` (10^15).
    fn normalized_parts(&self) -> (u64, i32, bool) {
        let (mut mantissa, mut exponent, negative) = match self {
            Amount::Xrp(d) => (d.unsigned_abs(), 0i32, *d < 0),
            Amount::Iou(a) => (a.mantissa(), a.exponent(), a.sign_bit()),
        };
        if mantissa != 0 {
            while mantissa < MIN_MANTISSA {
                mantissa *= 10;
                exponent -= 1;
            }
        }
        (mantissa, exponent, negative)
    }

    /// Build a result of the requested kind from a raw `(mantissa, exponent,
    /// negative)` triple. Native results truncate toward zero (rippled's
    /// `STAmount::canonicalize` for XRP); IOU results normalise.
    fn finish(
        mantissa: u64,
        exponent: i32,
        negative: bool,
        native: bool,
    ) -> Result<Amount, AmountError> {
        if native {
            Ok(Amount::Xrp(drops_floor(mantissa, exponent, negative)?))
        } else {
            Ok(Amount::Iou(IOUAmount::from_parts(
                mantissa, exponent, negative,
            )?))
        }
    }

    /// Non-rounding product (round-to-nearest via the `+7` fudge), result issue
    /// chosen by `native`. Mirrors `STAmount::multiply` (pre-Number regime).
    pub fn multiply(a: &Amount, b: &Amount, native: bool) -> Result<Amount, AmountError> {
        if a.is_zero() || b.is_zero() {
            return Ok(if native {
                Amount::Xrp(0)
            } else {
                Amount::Iou(IOUAmount::ZERO)
            });
        }
        let (m1, e1, n1) = a.normalized_parts();
        let (m2, e2, n2) = b.normalized_parts();
        let product = (m1 as u128) * (m2 as u128) / TEN_TO_14 + 7;
        Self::finish(u128_to_u64(product)?, e1 + e2 + 14, n1 != n2, native)
    }

    /// Non-rounding quotient (round-to-nearest via the `+5` fudge). Mirrors
    /// `STAmount::divide`.
    pub fn divide(num: &Amount, den: &Amount, native: bool) -> Result<Amount, AmountError> {
        if den.is_zero() {
            return Err(AmountError::DivisionByZero);
        }
        if num.is_zero() {
            return Ok(if native {
                Amount::Xrp(0)
            } else {
                Amount::Iou(IOUAmount::ZERO)
            });
        }
        let (m1, e1, n1) = num.normalized_parts();
        let (m2, e2, n2) = den.normalized_parts();
        let scaled = (m1 as u128) * TEN_TO_17 / (m2 as u128) + 5;
        Self::finish(u128_to_u64(scaled)?, e1 - e2 - 17, n1 != n2, native)
    }

    /// Directional product. `round_up` rounds away from zero; the native path
    /// uses rippled's legacy drops canonicalization. Mirrors `mulRound`.
    pub fn mul_round(
        a: &Amount,
        b: &Amount,
        native: bool,
        round_up: bool,
    ) -> Result<Amount, AmountError> {
        if a.is_zero() || b.is_zero() {
            return Ok(round_zero(
                native,
                round_up,
                a.is_negative() || b.is_negative(),
            ));
        }
        let (m1, e1, n1) = a.normalized_parts();
        let (m2, e2, n2) = b.normalized_parts();
        let negative = n1 != n2;
        let bias = if negative != round_up {
            TEN_TO_14 - 1
        } else {
            0
        };
        let raw = ((m1 as u128) * (m2 as u128) + bias) / TEN_TO_14;
        Self::round_finish(u128_to_u64(raw)?, e1 + e2 + 14, negative, native, round_up)
    }

    /// Directional quotient. Mirrors `divRound`.
    pub fn div_round(
        num: &Amount,
        den: &Amount,
        native: bool,
        round_up: bool,
    ) -> Result<Amount, AmountError> {
        if den.is_zero() {
            return Err(AmountError::DivisionByZero);
        }
        if num.is_zero() {
            return Ok(round_zero(native, round_up, num.is_negative()));
        }
        let (m1, e1, n1) = num.normalized_parts();
        let (m2, e2, n2) = den.normalized_parts();
        let negative = n1 != n2;
        let bias = if negative != round_up {
            (m2 as u128) - 1
        } else {
            0
        };
        let raw = ((m1 as u128) * TEN_TO_17 + bias) / (m2 as u128);
        Self::round_finish(u128_to_u64(raw)?, e1 - e2 - 17, negative, native, round_up)
    }

    /// Shared tail for `mul_round`/`div_round`: apply directional canonicalization,
    /// then bump a rounded-up zero to the smallest representable value.
    fn round_finish(
        mantissa: u64,
        exponent: i32,
        negative: bool,
        native: bool,
        round_up: bool,
    ) -> Result<Amount, AmountError> {
        if native {
            let drops = if negative != round_up {
                canonicalize_drops_round(mantissa, exponent)
            } else {
                scale_to_drops(mantissa, exponent)
            };
            let drops = u128_to_u64(drops)?;
            if round_up && !negative && drops == 0 {
                return Ok(Amount::Xrp(1));
            }
            return Ok(Amount::Xrp(if negative {
                -(drops as i64)
            } else {
                drops as i64
            }));
        }

        let (mut m, mut e) = (mantissa, exponent);
        if negative != round_up {
            canonicalize_round_iou(&mut m, &mut e);
        }
        let result = IOUAmount::from_parts(m, e, negative)?;
        if round_up && !negative && result.is_zero() {
            return Ok(Amount::Iou(IOUAmount::MIN_POSITIVE));
        }
        Ok(Amount::Iou(result))
    }
}

/// `u128 → u64` guarding against overflow.
fn u128_to_u64(v: u128) -> Result<u64, AmountError> {
    u64::try_from(v).map_err(|_| AmountError::Overflow)
}

/// Native zero result for the round helpers: a rounded-up positive zero becomes
/// one drop; otherwise plain zero.
fn round_zero(native: bool, round_up: bool, operand_negative: bool) -> Amount {
    if native {
        if round_up && !operand_negative {
            Amount::Xrp(1)
        } else {
            Amount::Xrp(0)
        }
    } else if round_up && !operand_negative {
        Amount::Iou(IOUAmount::MIN_POSITIVE)
    } else {
        Amount::Iou(IOUAmount::ZERO)
    }
}

/// Truncate a `(mantissa, exponent)` magnitude to drops (no rounding), the
/// native branch of `STAmount::canonicalize`.
fn scale_to_drops(mut value: u64, mut exponent: i32) -> u128 {
    if value == 0 || exponent <= -20 {
        return 0;
    }
    while exponent > 0 {
        value = value.saturating_mul(10);
        exponent -= 1;
    }
    while exponent < 0 {
        value /= 10;
        exponent += 1;
    }
    value as u128
}

/// Native result for the non-rounding `multiply`/`divide`: truncate to drops,
/// applying the sign and the native ceiling.
fn drops_floor(mantissa: u64, exponent: i32, negative: bool) -> Result<i64, AmountError> {
    let drops = scale_to_drops(mantissa, exponent);
    if drops > MAX_NATIVE_DROPS as u128 {
        return Err(AmountError::Overflow);
    }
    let drops = drops as i64;
    Ok(if negative { -drops } else { drops })
}

/// rippled's legacy `canonicalizeRound(native=true)`: the `loops>=2 ? 9 : 10`
/// drop-rounding quirk (fractional drop ≥ 0.1 rounds up).
fn canonicalize_drops_round(mut value: u64, mut exponent: i32) -> u128 {
    if value == 0 {
        return 0;
    }
    while exponent > 0 {
        value = value.saturating_mul(10);
        exponent -= 1;
    }
    if exponent < 0 {
        let mut loops = 0;
        while exponent < -1 {
            value /= 10;
            exponent += 1;
            loops += 1;
        }
        let adder = if loops >= 2 { 9 } else { 10 };
        value = (value + adder) / 10;
    }
    value as u128
}

/// rippled's `canonicalizeRound` IOU-overflow branch (mantissa above the
/// 16-digit range): `value += 9; value /= 10`.
fn canonicalize_round_iou(value: &mut u64, exponent: &mut i32) {
    if *value > MAX_MANTISSA {
        while *value > 10 * MAX_MANTISSA {
            *value /= 10;
            *exponent += 1;
        }
        *value += 9;
        *value /= 10;
        *exponent += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iou(mantissa: u64, exponent: i32, negative: bool) -> Amount {
        Amount::Iou(IOUAmount::from_parts(mantissa, exponent, negative).unwrap())
    }

    fn one() -> Amount {
        iou(MIN_MANTISSA, -15, false) // 1.0
    }

    #[test]
    fn xrp_classification() {
        assert!(Amount::Xrp(5).is_native());
        assert!(!one().is_native());
        assert!(Amount::Xrp(0).is_zero());
        assert!(Amount::Xrp(-3).is_negative());
    }

    #[test]
    fn iou_grossing_338500_transfer_fee() {
        // The #338500 oracle: 1 BTC grossed by a 1.002 transfer rate = 1.002 BTC.
        let amount = one();
        let rate = iou(1_002_000_000_000_000, -15, false); // 1.002
        let gross = Amount::multiply(&amount, &rate, false).unwrap();
        // 1.002 = 1002000000000000 e-15
        assert_eq!(gross, iou(1_002_000_000_000_000, -15, false));
    }

    #[test]
    fn iou_multiply_matches_iouamount() {
        let a = iou(1_500_000_000_000_000, -15, false); // 1.5
        let got = Amount::multiply(&a, &a, false).unwrap();
        assert_eq!(got, iou(2_250_000_000_000_000, -15, false)); // 2.25
    }

    #[test]
    fn native_multiply_truncates_to_drops() {
        // 44925 XRP (drops) × 1.0 = 44925000000 drops, native floor.
        let xrp = Amount::Xrp(44_925_000_000);
        let got = Amount::multiply(&xrp, &one(), true).unwrap();
        assert_eq!(got, Amount::Xrp(44_925_000_000));
    }

    #[test]
    fn native_divide_floor_vs_round() {
        // 10 drops / 3 → 3.333… ; non-rounding floor = 3 drops.
        let ten = Amount::Xrp(10);
        let three = iou(3_000_000_000_000_000, -15, false);
        let floor = Amount::divide(&ten, &three, true).unwrap();
        assert_eq!(floor, Amount::Xrp(3));
        // div_round up away from zero → 4 drops (fractional ≥ 0.1).
        let up = Amount::div_round(&ten, &three, true, true).unwrap();
        assert_eq!(up, Amount::Xrp(4));
    }

    #[test]
    fn native_mul_round_up_bumps_zero_to_one_drop() {
        let tiny = iou(MIN_MANTISSA, -30, false); // ~1e-15
        let got = Amount::mul_round(&tiny, &tiny, true, true).unwrap();
        assert_eq!(got, Amount::Xrp(1));
    }

    #[test]
    fn divide_by_zero_errors() {
        assert_eq!(
            Amount::divide(&one(), &Amount::Xrp(0), false),
            Err(AmountError::DivisionByZero)
        );
    }

    #[test]
    fn round_up_ge_round_down() {
        let a = iou(1_000_000_000_000_001, -15, false);
        let b = iou(3_000_000_000_000_000, -15, false);
        let up = Amount::div_round(&a, &b, false, true).unwrap();
        let down = Amount::div_round(&a, &b, false, false).unwrap();
        assert!(up.iou().unwrap() >= down.iou().unwrap());
    }

    #[test]
    fn drops_round_quirk_one_loop_rounds_up() {
        // exponent -1 → zero division loops, adder 10:
        // (15_000_000_000_000_000 + 10)/10 = 1_500_000_000_000_001 drops.
        assert_eq!(
            canonicalize_drops_round(15_000_000_000_000_000, -1),
            1_500_000_000_000_001
        );
        // exponent -3 → two division loops (value→150e12), adder 9:
        // (150_000_000_000_000 + 9)/10 = 15_000_000_000_000 drops.
        assert_eq!(
            canonicalize_drops_round(15_000_000_000_000_000, -3),
            15_000_000_000_000
        );
    }

    #[test]
    fn native_overflow_errors() {
        let huge = iou(MIN_MANTISSA, 40, false);
        assert_eq!(
            Amount::multiply(&huge, &huge, true),
            Err(AmountError::Overflow)
        );
    }
}
