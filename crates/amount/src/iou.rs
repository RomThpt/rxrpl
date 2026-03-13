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

        // 128-bit intermediate: (m1 * m2) / 10^14 + 7
        let product = (m1 as u128) * (m2 as u128);
        let result = product / (TEN_TO_14 as u128) + 7;

        if result > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        let result_negative = a.negative != b.negative;
        let result_exponent = e1 + e2 + 14;

        IOUAmount::from_parts(result as u64, result_exponent, result_negative)
    }

    /// Divide two IOU amounts.
    pub fn divide(num: &IOUAmount, den: &IOUAmount) -> Result<IOUAmount, AmountError> {
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

        // 128-bit intermediate: (m1 * 10^17) / m2 + 5
        let scaled = (m1 as u128) * TEN_TO_17;
        let result = scaled / (m2 as u128) + 5;

        if result > u64::MAX as u128 {
            return Err(AmountError::Overflow);
        }

        let result_negative = num.negative != den.negative;
        let result_exponent = e1 - e2 - 17;

        IOUAmount::from_parts(result as u64, result_exponent, result_negative)
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
                result = IOUAmount::from_parts(
                    result.mantissa + 1,
                    result.exponent,
                    result.negative,
                )?;
            } else if !round_up && negative {
                // Round away from zero for negative
                if result.is_zero() {
                    return Ok(IOUAmount::from_raw(MIN_MANTISSA, MIN_EXPONENT, true));
                }
                result = IOUAmount::from_parts(
                    result.mantissa + 1,
                    result.exponent,
                    result.negative,
                )?;
            }
        }

        Ok(result)
    }

    /// Add two IOU amounts.
    pub fn add(a: &IOUAmount, b: &IOUAmount) -> Result<IOUAmount, AmountError> {
        if a.is_zero() {
            return Ok(*b);
        }
        if b.is_zero() {
            return Ok(*a);
        }

        // Align exponents - bring the smaller-exponent value up
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

        if self.negative {
            mag.reverse()
        } else {
            mag
        }
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
        // 0.333333333333333... = 3333333333333333 * 10^-16
        assert_eq!(result.mantissa(), 3_333_333_333_333_333);
        assert_eq!(result.exponent(), -16);
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
}
