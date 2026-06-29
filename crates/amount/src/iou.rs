use crate::error::AmountError;

/// Minimum normalized mantissa (10^15).
pub const MIN_MANTISSA: u64 = 1_000_000_000_000_000;

/// Maximum normalized mantissa (10^16 - 1).
pub const MAX_MANTISSA: u64 = 9_999_999_999_999_999;

/// Minimum exponent for IOUAmount.
pub const MIN_EXPONENT: i32 = -96;

/// Maximum exponent for IOUAmount.
pub const MAX_EXPONENT: i32 = 80;

/// Sentinel exponent for zero values.
const ZERO_EXPONENT: i32 = -100;

/// 10^14, used as divisor in multiplication.
const TEN_TO_14: u64 = 100_000_000_000_000;

/// 10^17, used as multiplier in division.
const TEN_TO_17: u128 = 100_000_000_000_000_000;

/// An IOU amount with mantissa-exponent representation.
///
/// Matches rippled's IOUAmount / STAmount semantics:
/// - value = (if negative then -1 else 1) * mantissa * 10^exponent
/// - Normalized mantissa range: [10^15, 10^16 - 1]
/// - Exponent range: [-96, 80]
/// - Zero is represented with mantissa = 0, exponent = -100
#[derive(Clone, Copy, Debug)]
pub struct IOUAmount {
    mantissa: u64,
    exponent: i32,
    negative: bool,
}

impl IOUAmount {
    /// Zero amount.
    pub const ZERO: Self = Self {
        mantissa: 0,
        exponent: ZERO_EXPONENT,
        negative: false,
    };

    /// Smallest positive representable amount.
    pub const MIN_POSITIVE: Self = Self {
        mantissa: MIN_MANTISSA,
        exponent: MIN_EXPONENT,
        negative: false,
    };

    /// Create a new IOUAmount from mantissa, exponent, and sign.
    ///
    /// The value is automatically normalized.
    pub fn new(mantissa: i64, exponent: i32) -> Result<Self, AmountError> {
        let negative = mantissa < 0;
        let abs_mantissa = mantissa.unsigned_abs();
        let mut amt = Self {
            mantissa: abs_mantissa,
            exponent,
            negative,
        };
        amt.normalize()?;
        Ok(amt)
    }

    /// Create from unsigned mantissa, exponent, and explicit sign flag.
    pub fn from_parts(mantissa: u64, exponent: i32, negative: bool) -> Result<Self, AmountError> {
        let mut amt = Self {
            mantissa,
            exponent,
            negative,
        };
        amt.normalize()?;
        Ok(amt)
    }

    /// Create from a raw mantissa and exponent without normalization.
    /// Used internally when caller guarantees invariants.
    fn from_raw(mantissa: u64, exponent: i32, negative: bool) -> Self {
        Self {
            mantissa,
            exponent,
            negative,
        }
    }

    /// Returns true if this amount is zero.
    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    /// Returns true if this amount is negative.
    pub fn is_negative(&self) -> bool {
        self.negative && self.mantissa != 0
    }

    /// Returns the unsigned mantissa.
    pub fn mantissa(&self) -> u64 {
        self.mantissa
    }

    /// Returns the exponent.
    pub fn exponent(&self) -> i32 {
        self.exponent
    }

    /// Returns true if the amount is negative (sign bit).
    pub fn sign_bit(&self) -> bool {
        self.negative
    }

    /// Negate this amount.
    pub fn negate(mut self) -> Self {
        if self.mantissa != 0 {
            self.negative = !self.negative;
        }
        self
    }

    /// Return the absolute value.
    pub fn abs(mut self) -> Self {
        self.negative = false;
        self
    }

    /// Normalize mantissa and exponent to canonical form.
    fn normalize(&mut self) -> Result<(), AmountError> {
        if self.mantissa == 0 {
            self.exponent = ZERO_EXPONENT;
            self.negative = false;
            return Ok(());
        }

        // Scale up if mantissa is too small
        while self.mantissa < MIN_MANTISSA {
            self.mantissa *= 10;
            self.exponent -= 1;
        }

        // Scale down if mantissa is too large
        while self.mantissa > MAX_MANTISSA {
            if self.exponent >= MAX_EXPONENT {
                return Err(AmountError::Overflow);
            }
            self.mantissa /= 10;
            self.exponent += 1;
        }

        // Check for underflow
        if self.exponent < MIN_EXPONENT {
            self.mantissa = 0;
            self.exponent = ZERO_EXPONENT;
            self.negative = false;
            return Ok(());
        }

        if self.exponent > MAX_EXPONENT {
            return Err(AmountError::Overflow);
        }

        Ok(())
    }

    /// Multiply two IOU amounts.
    pub fn multiply(a: &IOUAmount, b: &IOUAmount) -> Result<IOUAmount, AmountError> {
        if a.is_zero() || b.is_zero() {
            return Ok(IOUAmount::ZERO);
        }

        let mut m1 = a.mantissa;
        let mut e1 = a.exponent;
        let mut m2 = b.mantissa;
        let mut e2 = b.exponent;

        // Ensure both mantissas are at least MIN_MANTISSA
        while m1 < MIN_MANTISSA {
            m1 *= 10;
            e1 -= 1;
        }
        while m2 < MIN_MANTISSA {
            m2 *= 10;
            e2 -= 1;
        }

        // Check that the combined exponent won't overflow i32 before normalization.
        // e1 + e2 + 14 must be representable; if it's wildly out of range,
        // normalization cannot bring it back within [MIN_EXPONENT, MAX_EXPONENT].
        let result_exponent = (e1 as i64) + (e2 as i64) + 14;
        if result_exponent > MAX_EXPONENT as i64 + 16 {
            // Even after normalizing a large mantissa down, the exponent would exceed MAX_EXPONENT
            return Err(AmountError::Overflow);
        }

        // 128-bit intermediate: (m1 * m2) / 10^14 + 7
        let product = (m1 as u128) * (m2 as u128);
        let result = product / (TEN_TO_14 as u128) + 7;

        if result > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        let result_negative = a.negative != b.negative;
        let result_exponent = result_exponent as i32;

        IOUAmount::from_parts(result as u64, result_exponent, result_negative)
    }

    /// Divide two IOU amounts with the **pre-`fixUniversalNumber`**
    /// canonicalisation: the `muldiv + 5` quotient is reduced to the canonical
    /// 16-digit mantissa by *truncation* (the divide-by-10 loop `from_parts`
    /// runs). This is the rounding mainnet used for order-book quality before
    /// the `Number`-switchover amendment, so e.g. ledger #316000's GBP/BTC offer
    /// lands on a `…200D` book directory rather than `…200E`.
    pub fn divide(num: &IOUAmount, den: &IOUAmount) -> Result<IOUAmount, AmountError> {
        Self::divide_canonical(num, den, false)
    }

    /// Divide two IOU amounts with the **post-`fixUniversalNumber`**
    /// canonicalisation: the same `muldiv + 5` quotient is reduced to 16 digits
    /// with round-half-to-**even**, exactly as rippled's `Number` does once the
    /// switchover amendment routes the `STAmount` constructor through `Number`.
    ///
    /// Both rules start from the identical `floor(num·10¹⁷/den) + 5`; they differ
    /// only in how the 17–18 digit pre-canonical mantissa is reduced to 16. A
    /// single fixed rule cannot reproduce both eras — the modern XRP/USD offer
    /// `04F5F33…` needs the half-even reduction to reach a `…7EA4` book
    /// directory (truncation lands one ULP low at `…7EA3`), while #316000 needs
    /// truncation. The choice is therefore amendment-gated by the caller, exactly
    /// as rippled gates it on `getSTNumberSwitchover()`.
    pub fn divide_round_even(num: &IOUAmount, den: &IOUAmount) -> Result<IOUAmount, AmountError> {
        Self::divide_canonical(num, den, true)
    }

    fn divide_canonical(
        num: &IOUAmount,
        den: &IOUAmount,
        round_even: bool,
    ) -> Result<IOUAmount, AmountError> {
        if den.is_zero() {
            return Err(AmountError::DivisionByZero);
        }
        if num.is_zero() {
            return Ok(IOUAmount::ZERO);
        }

        let mut m1 = num.mantissa;
        let mut e1 = num.exponent;
        let mut m2 = den.mantissa;
        let mut e2 = den.exponent;

        // Normalize both mantissas
        while m1 < MIN_MANTISSA {
            m1 *= 10;
            e1 -= 1;
        }
        while m2 < MIN_MANTISSA {
            m2 *= 10;
            e2 -= 1;
        }

        // rippled STAmount::divide: `muldiv(numVal, 10^17, denVal) + 5` — a floor
        // division plus a constant `+ 5` bias — then the STAmount constructor
        // canonicalizes the 17–18 digit product down to a 16-digit mantissa.
        let scaled = (m1 as u128) * TEN_TO_17;
        let result = scaled / (m2 as u128) + 5;

        let result_negative = num.negative != den.negative;
        let result_exponent = e1 - e2 - 17;

        if round_even {
            // Post-switchover: the ctor goes through `Number`, which reduces with
            // round-half-to-even.
            Self::from_parts_round_half_even(result, result_exponent, result_negative)
        } else {
            // Pre-switchover: the ctor truncates (the `from_parts` divide loop).
            if result > u64::MAX as u128 {
                return Err(AmountError::Overflow);
            }
            IOUAmount::from_parts(result as u64, result_exponent, result_negative)
        }
    }

    /// Build an IOU from a possibly over-precise mantissa, reducing to canonical
    /// 16-digit precision with round-half-to-even (rippled `Number` semantics:
    /// the discarded low digits are weighed against half, ties go to the even
    /// last digit). Used by the post-`fixUniversalNumber` divide path.
    fn from_parts_round_half_even(
        mut mantissa: u128,
        mut exponent: i32,
        negative: bool,
    ) -> Result<IOUAmount, AmountError> {
        if mantissa == 0 {
            return Ok(IOUAmount::ZERO);
        }
        let max = MAX_MANTISSA as u128;
        if mantissa > max {
            let mut drop = 0u32;
            let mut probe = mantissa;
            while probe > max {
                probe /= 10;
                drop += 1;
            }
            let div = 10u128.pow(drop);
            let q = mantissa / div;
            let r = mantissa % div;
            let half = div / 2;
            let round_up = r > half || (r == half && (q & 1 == 1));
            mantissa = if round_up { q + 1 } else { q };
            exponent += drop as i32;
            // Rounding up can carry into a 17th digit (e.g. 10^16); a trailing
            // power-of-ten reduction is exact, so no further rounding needed.
            while mantissa > max {
                mantissa /= 10;
                exponent += 1;
            }
        }
        if mantissa > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }
        IOUAmount::from_parts(mantissa as u64, exponent, negative)
    }

    /// Multiply two IOU amounts with rounding control.
    ///
    /// When `round_up` is true and the result would naturally round towards
    /// zero, we round away from zero instead.
    pub fn mul_round(
        a: &IOUAmount,
        b: &IOUAmount,
        round_up: bool,
    ) -> Result<IOUAmount, AmountError> {
        if a.is_zero() || b.is_zero() {
            if round_up && !(a.is_negative() || b.is_negative()) {
                // Round up from zero means smallest positive value
                return Ok(IOUAmount::MIN_POSITIVE);
            }
            return Ok(IOUAmount::ZERO);
        }

        let mut m1 = a.mantissa;
        let mut e1 = a.exponent;
        let mut m2 = b.mantissa;
        let mut e2 = b.exponent;

        while m1 < MIN_MANTISSA {
            m1 *= 10;
            e1 -= 1;
        }
        while m2 < MIN_MANTISSA {
            m2 *= 10;
            e2 -= 1;
        }

        let result_negative = a.negative != b.negative;
        let product = (m1 as u128) * (m2 as u128);

        // Rounding bias: if rounding direction differs from sign, add bias
        let bias = if result_negative != round_up {
            TEN_TO_14 as u128 - 1
        } else {
            0
        };
        let result = (product + bias) / (TEN_TO_14 as u128);

        if result > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        let mut amount = result as u64;
        let mut offset = e1 + e2 + 14;

        // Canonicalize rounding for oversized mantissa
        if result_negative != round_up {
            canonicalize_round(&mut amount, &mut offset);
        }

        let result = IOUAmount::from_parts(amount, offset, result_negative)?;

        // If rounding up and result is zero, return smallest positive
        if round_up && !result_negative && result.is_zero() {
            return Ok(IOUAmount::MIN_POSITIVE);
        }

        Ok(result)
    }

    /// Divide two IOU amounts with rounding control.
    pub fn div_round(
        num: &IOUAmount,
        den: &IOUAmount,
        round_up: bool,
    ) -> Result<IOUAmount, AmountError> {
        if den.is_zero() {
            return Err(AmountError::DivisionByZero);
        }
        if num.is_zero() {
            if round_up && !num.is_negative() {
                return Ok(IOUAmount::MIN_POSITIVE);
            }
            return Ok(IOUAmount::ZERO);
        }

        let mut m1 = num.mantissa;
        let mut e1 = num.exponent;
        let mut m2 = den.mantissa;
        let mut e2 = den.exponent;

        while m1 < MIN_MANTISSA {
            m1 *= 10;
            e1 -= 1;
        }
        while m2 < MIN_MANTISSA {
            m2 *= 10;
            e2 -= 1;
        }

        let result_negative = num.negative != den.negative;

        let scaled = (m1 as u128) * TEN_TO_17;
        let bias = if result_negative != round_up {
            (m2 as u128) - 1
        } else {
            0
        };
        let result = (scaled + bias) / (m2 as u128);

        if result > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        let mut amount = result as u64;
        let mut offset = e1 - e2 - 17;

        if result_negative != round_up {
            canonicalize_round(&mut amount, &mut offset);
        }

        let result = IOUAmount::from_parts(amount, offset, result_negative)?;

        if round_up && !result_negative && result.is_zero() {
            return Ok(IOUAmount::MIN_POSITIVE);
        }

        Ok(result)
    }

    /// Multiply by a ratio (numerator / denominator).
    ///
    /// Used for transfer fees and other ratio-based calculations.
    /// Uses 128-bit intermediates for precision.
    pub fn mul_ratio(
        &self,
        numerator: u32,
        denominator: u32,
        round_up: bool,
    ) -> Result<IOUAmount, AmountError> {
        if denominator == 0 {
            return Err(AmountError::DivisionByZero);
        }
        if self.is_zero() || numerator == 0 {
            return Ok(IOUAmount::ZERO);
        }

        // 128-bit: mantissa * numerator
        let mul = (self.mantissa as u128) * (numerator as u128);
        let low = mul / (denominator as u128);
        let rem = mul % (denominator as u128);

        let has_rem = rem != 0;
        let mut result_mantissa = low as u64;
        let mut result_exponent = self.exponent;

        // If result is too large for u64, scale down
        while result_mantissa > MAX_MANTISSA {
            result_mantissa /= 10;
            result_exponent += 1;
        }

        let negative = self.negative;
        let mut result = IOUAmount::from_parts(result_mantissa, result_exponent, negative)?;

        // Apply rounding
        if has_rem {
            if round_up && !negative {
                // Round away from zero for positive
                if result.is_zero() {
                    return Ok(IOUAmount::MIN_POSITIVE);
                }
                result =
                    IOUAmount::from_parts(result.mantissa + 1, result.exponent, result.negative)?;
            } else if !round_up && negative {
                // Round away from zero for negative
                if result.is_zero() {
                    return Ok(IOUAmount::from_raw(MIN_MANTISSA, MIN_EXPONENT, true));
                }
                result =
                    IOUAmount::from_parts(result.mantissa + 1, result.exponent, result.negative)?;
            }
        }

        Ok(result)
    }

    /// Parse a decimal string (e.g. `"-3.022389776875825"`) exactly, with no
    /// floating point. Significant digits beyond 16 are dropped by the
    /// normalization loop, matching rippled's STAmount parsing for the values
    /// stored in trust-line balances.
    pub fn from_decimal_string(s: &str) -> Result<IOUAmount, AmountError> {
        let s = s.trim();
        let negative = s.starts_with('-');
        let body = s.trim_start_matches(['-', '+']);
        let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
        if int_part.is_empty() && frac_part.is_empty() {
            return Err(AmountError::Overflow);
        }
        if !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_digit())
        {
            return Err(AmountError::Overflow);
        }
        let digits = format!("{int_part}{frac_part}");
        let trimmed = digits.trim_start_matches('0');
        if trimmed.is_empty() {
            return Ok(IOUAmount::ZERO);
        }
        // Keep at most 17 leading digits so the mantissa fits u64 before the
        // normalize loop trims it to the canonical 16.
        let kept = &trimmed[..trimmed.len().min(17)];
        let dropped = trimmed.len() - kept.len();
        let mantissa: u64 = kept.parse().map_err(|_| AmountError::Overflow)?;
        let exponent = -(frac_part.len() as i32) + dropped as i32;
        IOUAmount::from_parts(mantissa, exponent, negative)
    }

    /// Render as a plain decimal string that [`IOUAmount::from_decimal_string`]
    /// (and rippled's STAmount parser) round-trips back to the same value.
    /// No exponent form, no trailing fractional zeros.
    pub fn to_decimal_string(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let sign = if self.negative { "-" } else { "" };
        let digits = self.mantissa.to_string();
        let exp = self.exponent;
        if exp >= 0 {
            return format!("{sign}{digits}{}", "0".repeat(exp as usize));
        }
        let frac = (-exp) as usize;
        let body = if frac >= digits.len() {
            let zeros = "0".repeat(frac - digits.len());
            let frac_digits = format!("{zeros}{digits}");
            format!("0.{}", frac_digits.trim_end_matches('0'))
        } else {
            let point = digits.len() - frac;
            let (int_part, frac_part) = digits.split_at(point);
            let frac_part = frac_part.trim_end_matches('0');
            if frac_part.is_empty() {
                int_part.to_string()
            } else {
                format!("{int_part}.{frac_part}")
            }
        };
        format!("{sign}{body}")
    }

    /// Add two IOU amounts.
    pub fn add(a: &IOUAmount, b: &IOUAmount) -> Result<IOUAmount, AmountError> {
        if a.is_zero() {
            return Ok(*b);
        }
        if b.is_zero() {
            return Ok(*a);
        }

        // Align exponents by dividing the smaller-exponent mantissa up.
        // NOTE: When exponents differ wildly, the smaller value's mantissa may
        // be divided down to 0, effectively dropping it. This matches rippled
        // behavior -- the precision loss is inherent to the fixed-mantissa format.
        let (mut m1, mut e1, n1) = (a.mantissa as i128, a.exponent, a.negative);
        let (mut m2, mut e2, n2) = (b.mantissa as i128, b.exponent, b.negative);

        while e1 < e2 {
            m1 /= 10;
            e1 += 1;
            if m1 == 0 {
                return Ok(*b);
            }
        }
        while e2 < e1 {
            m2 /= 10;
            e2 += 1;
            if m2 == 0 {
                return Ok(*a);
            }
        }

        // Apply signs
        let v1 = if n1 { -m1 } else { m1 };
        let v2 = if n2 { -m2 } else { m2 };

        let sum = v1 + v2;
        if sum == 0 {
            return Ok(IOUAmount::ZERO);
        }

        let negative = sum < 0;
        let abs_sum = sum.unsigned_abs();

        if abs_sum > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        IOUAmount::from_parts(abs_sum as u64, e1, negative)
    }

    /// Subtract b from a.
    pub fn sub(a: &IOUAmount, b: &IOUAmount) -> Result<IOUAmount, AmountError> {
        Self::add(a, &b.negate())
    }

    /// Add with round-to-nearest (half-up) to the 16-digit mantissa, matching
    /// rippled's `Number`/post-amendment STAmount addition. Unlike [`add`],
    /// which aligns by truncating the smaller term (the pre-`Number` behavior),
    /// this adds at the lower exponent — losing no digit before the sum — then
    /// rounds. Used for IOU balance updates outside the legacy crossing path.
    pub fn add_round(a: &IOUAmount, b: &IOUAmount) -> Result<IOUAmount, AmountError> {
        if a.is_zero() {
            return Ok(*b);
        }
        if b.is_zero() {
            return Ok(*a);
        }
        let lo = a.exponent.min(b.exponent);
        let hi = a.exponent.max(b.exponent);
        if (hi - lo) > 19 {
            return Ok(if a.exponent == hi { *a } else { *b });
        }
        let ma = (a.mantissa as i128) * 10i128.pow((a.exponent - lo) as u32);
        let mb = (b.mantissa as i128) * 10i128.pow((b.exponent - lo) as u32);
        let va = if a.negative { -ma } else { ma };
        let vb = if b.negative { -mb } else { mb };
        let sum = va + vb;
        if sum == 0 {
            return Ok(IOUAmount::ZERO);
        }
        let negative = sum < 0;
        let mut mag = sum.unsigned_abs();
        let mut exp = lo;
        const MAXM: u128 = MAX_MANTISSA as u128;
        if mag > MAXM {
            let mut digits_over = 0u32;
            let mut probe = mag;
            while probe > MAXM {
                probe /= 10;
                digits_over += 1;
            }
            let div = 10u128.pow(digits_over);
            let rem = mag % div;
            let mut rounded = mag / div;
            if rem * 2 >= div {
                rounded += 1;
            }
            exp += digits_over as i32;
            if rounded > MAXM {
                rounded /= 10;
                exp += 1;
            }
            mag = rounded;
        }
        IOUAmount::from_parts(mag as u64, exp, negative)
    }
}

/// Canonicalize rounding for oversized mantissa values.
///
/// When we've rounded up and the mantissa exceeds MAX_MANTISSA,
/// we need to adjust mantissa and exponent while preserving the
/// rounding direction.
fn canonicalize_round(amount: &mut u64, offset: &mut i32) {
    if *amount > MAX_MANTISSA {
        while *amount > 10 * MAX_MANTISSA {
            *amount /= 10;
            *offset += 1;
        }
        *amount += 9;
        *amount /= 10;
        *offset += 1;
    }
}

impl PartialEq for IOUAmount {
    fn eq(&self, other: &Self) -> bool {
        if self.is_zero() && other.is_zero() {
            return true;
        }
        self.mantissa == other.mantissa
            && self.exponent == other.exponent
            && self.negative == other.negative
    }
}

impl Eq for IOUAmount {}

impl PartialOrd for IOUAmount {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for IOUAmount {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;

        // Handle zeros
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        if self.is_zero() {
            return if other.negative {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        if other.is_zero() {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        // Different signs
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        // Same sign: compare magnitude, flip if negative
        let mag = if self.exponent != other.exponent {
            self.exponent.cmp(&other.exponent)
        } else {
            self.mantissa.cmp(&other.mantissa)
        };

        if self.negative { mag.reverse() } else { mag }
    }
}

impl std::fmt::Display for IOUAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_zero() {
            return write!(f, "0");
        }
        if self.negative {
            write!(f, "-")?;
        }
        write!(f, "{}e{}", self.mantissa, self.exponent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_amount() {
        let z = IOUAmount::ZERO;
        assert!(z.is_zero());
        assert!(!z.is_negative());
        assert_eq!(z.mantissa(), 0);
        assert_eq!(z.exponent(), ZERO_EXPONENT);
    }

    #[test]
    fn new_normalizes() {
        // 1.0 = 1000000000000000 * 10^-15
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        assert_eq!(a.mantissa(), MIN_MANTISSA);
        assert_eq!(a.exponent(), -15);

        // Small value gets scaled up
        let b = IOUAmount::new(42, 0).unwrap();
        assert!(b.mantissa() >= MIN_MANTISSA);
        assert!(b.mantissa() <= MAX_MANTISSA);
    }

    #[test]
    fn negative_amount() {
        let a = IOUAmount::new(-1_000_000_000_000_000, -15).unwrap();
        assert!(a.is_negative());
        assert_eq!(a.mantissa(), MIN_MANTISSA);
    }

    #[test]
    fn negate() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let neg = a.negate();
        assert!(neg.is_negative());
        let pos = neg.negate();
        assert!(!pos.is_negative());
        assert_eq!(a, pos);
    }

    #[test]
    fn zero_negate_stays_zero() {
        let z = IOUAmount::ZERO.negate();
        assert!(z.is_zero());
        assert!(!z.is_negative());
    }

    #[test]
    fn multiply_basic() {
        // 2.0 * 3.0 = 6.0
        let a = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(3_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::multiply(&a, &b).unwrap();
        // 6.0 = 6000000000000000 * 10^-15
        assert_eq!(result.mantissa(), 6_000_000_000_000_000);
        assert_eq!(result.exponent(), -15);
    }

    #[test]
    fn multiply_by_zero() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::multiply(&a, &IOUAmount::ZERO).unwrap();
        assert!(result.is_zero());
    }

    #[test]
    fn multiply_signs() {
        let pos = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();
        let neg = IOUAmount::new(-3_000_000_000_000_000, -15).unwrap();

        let r1 = IOUAmount::multiply(&pos, &neg).unwrap();
        assert!(r1.is_negative());

        let r2 = IOUAmount::multiply(&neg, &neg).unwrap();
        assert!(!r2.is_negative());
    }

    #[test]
    fn divide_basic() {
        // 6.0 / 2.0 = 3.0
        let a = IOUAmount::new(6_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::divide(&a, &b).unwrap();
        assert_eq!(result.mantissa(), 3_000_000_000_000_000);
        assert_eq!(result.exponent(), -15);
    }

    #[test]
    fn divide_by_zero_error() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        assert_eq!(
            IOUAmount::divide(&a, &IOUAmount::ZERO),
            Err(AmountError::DivisionByZero)
        );
    }

    #[test]
    fn divide_zero_numerator() {
        let b = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::divide(&IOUAmount::ZERO, &b).unwrap();
        assert!(result.is_zero());
    }

    #[test]
    fn ordering() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();
        let neg = IOUAmount::new(-1_000_000_000_000_000, -15).unwrap();

        assert!(a < b);
        assert!(neg < a);
        assert!(neg < IOUAmount::ZERO);
        assert!(IOUAmount::ZERO < a);
        assert_eq!(IOUAmount::ZERO, IOUAmount::ZERO);
    }

    #[test]
    fn add_same_sign() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::add(&a, &b).unwrap();
        assert_eq!(result.mantissa(), 3_000_000_000_000_000);
        assert_eq!(result.exponent(), -15);
    }

    #[test]
    fn add_opposite_signs_cancel() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(-1_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::add(&a, &b).unwrap();
        assert!(result.is_zero());
    }

    #[test]
    fn sub_basic() {
        let a = IOUAmount::new(3_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::sub(&a, &b).unwrap();
        assert_eq!(result.mantissa(), 2_000_000_000_000_000);
        assert_eq!(result.exponent(), -15);
    }

    #[test]
    fn mul_round_up() {
        let a = IOUAmount::new(1_000_000_000_000_001, -15).unwrap();
        let b = IOUAmount::new(1_000_000_000_000_001, -15).unwrap();
        let r_up = IOUAmount::mul_round(&a, &b, true).unwrap();
        let r_down = IOUAmount::mul_round(&a, &b, false).unwrap();
        assert!(r_up >= r_down);
    }

    #[test]
    fn div_round_up() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(3_000_000_000_000_000, -15).unwrap();
        let r_up = IOUAmount::div_round(&a, &b, true).unwrap();
        let r_down = IOUAmount::div_round(&a, &b, false).unwrap();
        assert!(r_up >= r_down);
    }

    #[test]
    fn mul_ratio_basic() {
        // 100 * (3/4) = 75
        let a = IOUAmount::new(1_000_000_000_000_000, -13).unwrap(); // 100
        let result = a.mul_ratio(3, 4, false).unwrap();
        // Should be 75 = 7500000000000000 * 10^-14
        assert!(!result.is_zero());
        assert!(!result.is_negative());
    }

    #[test]
    fn mul_ratio_division_by_zero() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        assert_eq!(a.mul_ratio(1, 0, false), Err(AmountError::DivisionByZero));
    }

    #[test]
    fn mul_ratio_zero_numerator() {
        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let result = a.mul_ratio(0, 1, false).unwrap();
        assert!(result.is_zero());
    }

    #[test]
    fn display() {
        let z = IOUAmount::ZERO;
        assert_eq!(z.to_string(), "0");

        let a = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        assert_eq!(a.to_string(), "1000000000000000e-15");

        let neg = IOUAmount::new(-5_000_000_000_000_000, -15).unwrap();
        assert_eq!(neg.to_string(), "-5000000000000000e-15");
    }

    #[test]
    fn underflow_to_zero() {
        // Very small value that underflows
        let a = IOUAmount::from_parts(1, -200, false).unwrap();
        assert!(a.is_zero());
    }

    #[test]
    fn multiply_preserves_precision() {
        // 1.5 * 1.5 = 2.25
        let a = IOUAmount::new(1_500_000_000_000_000, -15).unwrap();
        let result = IOUAmount::multiply(&a, &a).unwrap();
        // 2.25 = 2250000000000000 * 10^-15
        assert_eq!(result.mantissa(), 2_250_000_000_000_000);
        assert_eq!(result.exponent(), -15);
    }

    #[test]
    fn divide_preserves_precision() {
        // 1.0 / 3.0 should have 15-digit precision
        let one = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let three = IOUAmount::new(3_000_000_000_000_000, -15).unwrap();
        let result = IOUAmount::divide(&one, &three).unwrap();
        // 0.3333... reduces to 16 digits via rippled's directed rounding: the
        // floor quotient + 5 bias yields ...338, which the *truncating*
        // canonicalize drops to ...333 (round-half-up of the true 0.3333...3,
        // whose 17th digit is below the half point).
        assert_eq!(result.mantissa(), 3_333_333_333_333_333);
        assert_eq!(result.exponent(), -16);
    }

    #[test]
    fn divide_truncates_pre_switchover_316000() {
        // Ledger #316000 OfferCreate (pre-fixUniversalNumber): the book quality
        // GBP 277.167203027 / BTC 9.982465241 must reduce by TRUNCATION to the
        // mantissa rippled stored (rate byte tail …200D), not the half-even
        // value (…200E).
        let gbp = IOUAmount::from_parts(2_771_672_030_270_000, -13, false).unwrap();
        let btc = IOUAmount::from_parts(9_982_465_241_000_000, -15, false).unwrap();
        let q = IOUAmount::divide(&gbp, &btc).unwrap();
        assert_eq!(q.mantissa(), 2_776_540_627_345_421);
        assert_eq!(q.exponent(), -14);
    }

    #[test]
    fn divide_round_even_post_switchover_modern() {
        // Modern OfferCreate 04F5F33… (post-fixUniversalNumber): the book quality
        // XRP 1000000000000 drops / USD 1025720 must reduce with round-half-to-
        // even to the mantissa rippled stored (rate byte tail …7EA4); truncation
        // would land one ULP low at …7EA3.
        let xrp = IOUAmount::from_parts(1_000_000_000_000_000, -3, false).unwrap();
        let usd = IOUAmount::from_parts(1_025_720_000_000_000, -9, false).unwrap();
        let q = IOUAmount::divide_round_even(&xrp, &usd).unwrap();
        assert_eq!(q.mantissa(), 9_749_249_307_803_300);
        assert_eq!(q.exponent(), -10);
        // The truncating divide lands one ULP low.
        let qt = IOUAmount::divide(&xrp, &usd).unwrap();
        assert_eq!(qt.mantissa(), 9_749_249_307_803_299);
    }

    #[test]
    fn large_exponent_multiply() {
        // 10^30 * 10^30 = 10^60 (within max IOU range of ~10^96)
        let a = IOUAmount::new(1_000_000_000_000_000, 15).unwrap(); // 10^30
        let b = IOUAmount::new(1_000_000_000_000_000, 15).unwrap();
        let result = IOUAmount::multiply(&a, &b).unwrap();
        assert!(!result.is_zero());
        assert_eq!(result.exponent(), 45); // 10^60
    }

    #[test]
    fn overflow_multiply() {
        // 10^55 * 10^55 = 10^110 > max IOU range
        let a = IOUAmount::new(1_000_000_000_000_000, 40).unwrap();
        let b = IOUAmount::new(1_000_000_000_000_000, 40).unwrap();
        let result = IOUAmount::multiply(&a, &b);
        assert!(result.is_err());
    }

    #[test]
    fn equality_for_zeros() {
        let z1 = IOUAmount::ZERO;
        let z2 = IOUAmount::new(0, 0).unwrap();
        assert_eq!(z1, z2);
    }

    // --- Edge case tests ---

    #[test]
    fn add_wildly_different_exponents() {
        // When exponents differ greatly, the tiny value is lost during alignment.
        // This matches rippled behavior: precision loss is inherent to the format.
        let big = IOUAmount::from_parts(1_000_000_000_000_000, 80, false).unwrap();
        let tiny = IOUAmount::from_parts(1_000_000_000_000_000, -96, false).unwrap();
        let result = IOUAmount::add(&big, &tiny).unwrap();
        assert_eq!(result, big);
    }

    #[test]
    fn sub_equal_values_clears_negative() {
        let a = IOUAmount::from_parts(5_000_000_000_000_000, 10, false).unwrap();
        let result = IOUAmount::sub(&a, &a).unwrap();
        assert!(result.is_zero());
        assert!(!result.is_negative());
    }

    #[test]
    fn sub_negative_result() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let b = IOUAmount::from_parts(2_000_000_000_000_000, 0, false).unwrap();
        let result = IOUAmount::sub(&a, &b).unwrap();
        assert!(result.is_negative());
    }

    #[test]
    fn multiply_near_max_exponent() {
        // Product: mantissa = (10^15 * 10^15)/10^14 + 7 = 10^16 + 7
        // After normalization: mantissa/10 -> ~10^15, exp = 33+33+14+1 = 81 > MAX(80) -> Overflow
        // Use smaller exponents so result fits: 32+33+14 = 79
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 32, false).unwrap();
        let b = IOUAmount::from_parts(1_000_000_000_000_000, 33, false).unwrap();
        let result = IOUAmount::multiply(&a, &b);
        assert!(result.is_ok());
        let val = result.unwrap();
        // Mantissa normalizes: (10^16+7)/10 = 10^15, exp = 79+1 = 80
        assert_eq!(val.exponent(), 80);
    }

    #[test]
    fn multiply_overflow_exponent() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 50, false).unwrap();
        let b = IOUAmount::from_parts(1_000_000_000_000_000, 50, false).unwrap();
        // exp would be 50 + 50 + 14 = 114, well beyond MAX_EXPONENT
        let result = IOUAmount::multiply(&a, &b);
        assert!(matches!(result, Err(AmountError::Overflow)));
    }

    #[test]
    fn divide_very_small_by_very_large() {
        let small = IOUAmount::from_parts(1_000_000_000_000_000, -96, false).unwrap();
        let large = IOUAmount::from_parts(9_999_999_999_999_999, 80, false).unwrap();
        let result = IOUAmount::divide(&small, &large);
        // The result exponent would be -96 - 80 - 17 = -193, which underflows to zero
        assert!(result.is_ok());
        assert!(result.unwrap().is_zero());
    }

    #[test]
    fn mul_round_preserves_direction() {
        let a = IOUAmount::from_parts(3_333_333_333_333_333, 0, false).unwrap();
        let b = IOUAmount::from_parts(3_000_000_000_000_000, 0, false).unwrap();
        let up = IOUAmount::mul_round(&a, &b, true).unwrap();
        let down = IOUAmount::mul_round(&a, &b, false).unwrap();
        assert!(up >= down);
    }

    #[test]
    fn mul_round_canonicalize_does_not_undo_rounding() {
        // Verify that canonicalize_round and subsequent normalize do not
        // undo the rounding bias. The round-up result must always be >= round-down.
        let a = IOUAmount::from_parts(9_999_999_999_999_999, 0, false).unwrap();
        let b = IOUAmount::from_parts(9_999_999_999_999_999, 0, false).unwrap();
        let up = IOUAmount::mul_round(&a, &b, true).unwrap();
        let down = IOUAmount::mul_round(&a, &b, false).unwrap();
        assert!(up >= down);

        // Also test with values that produce remainder in division
        let c = IOUAmount::from_parts(1_000_000_000_000_001, 0, false).unwrap();
        let d = IOUAmount::from_parts(3_000_000_000_000_000, 0, false).unwrap();
        let up2 = IOUAmount::mul_round(&c, &d, true).unwrap();
        let down2 = IOUAmount::mul_round(&c, &d, false).unwrap();
        assert!(up2 >= down2);
    }

    #[test]
    fn div_round_canonicalize_does_not_undo_rounding() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let b = IOUAmount::from_parts(3_000_000_000_000_000, 0, false).unwrap();
        let up = IOUAmount::div_round(&a, &b, true).unwrap();
        let down = IOUAmount::div_round(&a, &b, false).unwrap();
        assert!(up >= down);
    }

    #[test]
    fn mul_ratio_small_numerator() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let result = a.mul_ratio(1, 3, false).unwrap();
        assert!(!result.is_zero());
    }

    #[test]
    fn mul_ratio_division_by_zero_returns_error() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        assert!(matches!(
            a.mul_ratio(1, 0, false),
            Err(AmountError::DivisionByZero)
        ));
    }

    #[test]
    fn negative_times_negative_is_positive() {
        let a = IOUAmount::from_parts(2_000_000_000_000_000, 0, true).unwrap();
        let b = IOUAmount::from_parts(3_000_000_000_000_000, 0, true).unwrap();
        let result = IOUAmount::multiply(&a, &b).unwrap();
        assert!(!result.is_negative());
    }

    #[test]
    fn add_zero_with_positive() {
        let zero = IOUAmount::ZERO;
        let pos = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let result = IOUAmount::add(&zero, &pos).unwrap();
        assert_eq!(result, pos);
    }

    #[test]
    fn add_positive_with_zero() {
        let pos = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let zero = IOUAmount::ZERO;
        let result = IOUAmount::add(&pos, &zero).unwrap();
        assert_eq!(result, pos);
    }

    #[test]
    fn ordering_negative_vs_positive() {
        let neg = IOUAmount::from_parts(5_000_000_000_000_000, 10, true).unwrap();
        let pos = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        assert!(neg < pos);
    }

    #[test]
    fn ordering_same_mantissa_different_exponent() {
        let a = IOUAmount::from_parts(1_000_000_000_000_000, 5, false).unwrap();
        let b = IOUAmount::from_parts(1_000_000_000_000_000, 10, false).unwrap();
        assert!(a < b);
    }

    #[test]
    fn multiply_positive_by_negative() {
        let pos = IOUAmount::from_parts(2_000_000_000_000_000, 0, false).unwrap();
        let neg = IOUAmount::from_parts(3_000_000_000_000_000, 0, true).unwrap();
        let result = IOUAmount::multiply(&pos, &neg).unwrap();
        assert!(result.is_negative());
    }

    #[test]
    fn divide_negative_by_negative() {
        let a = IOUAmount::from_parts(6_000_000_000_000_000, 0, true).unwrap();
        let b = IOUAmount::from_parts(2_000_000_000_000_000, 0, true).unwrap();
        let result = IOUAmount::divide(&a, &b).unwrap();
        assert!(!result.is_negative());
    }

    #[test]
    fn sub_from_zero() {
        let zero = IOUAmount::ZERO;
        let pos = IOUAmount::from_parts(1_000_000_000_000_000, 0, false).unwrap();
        let result = IOUAmount::sub(&zero, &pos).unwrap();
        assert!(result.is_negative());
    }

    #[test]
    fn mul_round_zero_inputs_round_up() {
        // When both inputs are zero and round_up is true with positive signs,
        // should return MIN_POSITIVE
        let result = IOUAmount::mul_round(&IOUAmount::ZERO, &IOUAmount::ZERO, true).unwrap();
        assert_eq!(result, IOUAmount::MIN_POSITIVE);
    }

    #[test]
    fn decimal_roundtrip_338500_balances() {
        // The #338500 RippleState balances must round-trip byte-exactly.
        for s in [
            "-3.022389776875825",
            "-2.020389776875825",
            "1.002",
            "-1",
            "149.562159591",
            "0",
        ] {
            let a = IOUAmount::from_decimal_string(s).unwrap();
            let rendered = a.to_decimal_string();
            let b = IOUAmount::from_decimal_string(&rendered).unwrap();
            assert_eq!(a, b, "round-trip mismatch for {s} -> {rendered}");
        }
    }

    #[test]
    fn decimal_string_forms() {
        assert_eq!(
            IOUAmount::from_decimal_string("1.002")
                .unwrap()
                .to_decimal_string(),
            "1.002"
        );
        assert_eq!(
            IOUAmount::from_decimal_string("100")
                .unwrap()
                .to_decimal_string(),
            "100"
        );
        assert_eq!(
            IOUAmount::from_decimal_string("-1")
                .unwrap()
                .to_decimal_string(),
            "-1"
        );
        assert_eq!(
            IOUAmount::from_decimal_string("0.5")
                .unwrap()
                .to_decimal_string(),
            "0.5"
        );
        assert_eq!(
            IOUAmount::from_decimal_string("0")
                .unwrap()
                .to_decimal_string(),
            "0"
        );
    }

    #[test]
    fn decimal_delta_338500_owner() {
        // Owner BTC line: -3.022389776875825 + 1.002 (debit grossed) = -2.020389776875825.
        let before = IOUAmount::from_decimal_string("-3.022389776875825").unwrap();
        let gross = IOUAmount::from_decimal_string("1.002").unwrap();
        let after = IOUAmount::add(&before, &gross).unwrap();
        assert_eq!(after.to_decimal_string(), "-2.020389776875825");
    }

    #[test]
    fn abs_of_negative() {
        let neg = IOUAmount::from_parts(5_000_000_000_000_000, 0, true).unwrap();
        let pos = neg.abs();
        assert!(!pos.is_negative());
        assert_eq!(pos.mantissa(), 5_000_000_000_000_000);
    }
}
