use crate::error::AmountError;
use crate::iou::IOUAmount;
#[cfg(test)]
use crate::iou::MIN_MANTISSA;

/// Encode a quality (exchange rate) as a packed u64.
///
/// Quality = in / out, encoded with 8-bit exponent and 56-bit mantissa.
/// Lower quality values represent better exchange rates.
///
/// The exponent is biased by +100 so negative exponents become positive.
/// This allows lexicographic sorting: lower quality = better rate.
pub fn get_rate(offer_in: &IOUAmount, offer_out: &IOUAmount) -> Result<u64, AmountError> {
    if offer_out.is_zero() {
        return Ok(0); // Worthless offer
    }

    let rate = IOUAmount::divide(offer_in, offer_out)?;
    if rate.is_zero() {
        return Ok(0); // Offer too good to represent
    }

    // Pack: high 8 bits = exponent + 100, low 56 bits = mantissa
    let exp_biased = (rate.exponent() + 100) as u64;
    Ok((exp_biased << 56) | rate.mantissa())
}

/// Decode a packed quality u64 back into an IOUAmount.
///
/// Returns the exchange rate as an IOUAmount.
pub fn from_rate(rate: u64) -> Result<IOUAmount, AmountError> {
    if rate == 0 {
        return Ok(IOUAmount::ZERO);
    }

    let mantissa = rate & 0x00FF_FFFF_FFFF_FFFF;
    let exp_biased = (rate >> 56) as i32;
    let exponent = exp_biased - 100;

    IOUAmount::from_parts(mantissa, exponent, false)
}

/// Compare two quality values.
///
/// Returns true if quality `a` represents a better (lower) rate than `b`.
/// A lower rate means the taker gets more for less.
pub fn is_better_quality(a: u64, b: u64) -> bool {
    // Special case: zero quality means worthless
    if a == 0 {
        return false;
    }
    if b == 0 {
        return true;
    }
    a < b
}

/// Compute the quality for an offer given taker-pays and taker-gets amounts.
///
/// Quality = taker_pays / taker_gets. Lower is better for the taker.
pub fn offer_quality(taker_pays: &IOUAmount, taker_gets: &IOUAmount) -> Result<u64, AmountError> {
    get_rate(taker_pays, taker_gets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_roundtrip() {
        let offer_in = IOUAmount::new(2_000_000_000_000_000, -15).unwrap(); // 2.0
        let offer_out = IOUAmount::new(1_000_000_000_000_000, -15).unwrap(); // 1.0
        let rate = get_rate(&offer_in, &offer_out).unwrap();
        assert_ne!(rate, 0);

        let decoded = from_rate(rate).unwrap();
        assert_eq!(decoded.mantissa(), 2_000_000_000_000_000);
    }

    #[test]
    fn zero_output_returns_zero() {
        let offer_in = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let rate = get_rate(&offer_in, &IOUAmount::ZERO).unwrap();
        assert_eq!(rate, 0);
    }

    #[test]
    fn better_quality_comparison() {
        // Rate of 1.0 is better than rate of 2.0
        let one = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let two = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();

        let rate_1_1 = get_rate(&one, &one).unwrap(); // quality = 1.0
        let rate_2_1 = get_rate(&two, &one).unwrap(); // quality = 2.0

        assert!(is_better_quality(rate_1_1, rate_2_1));
        assert!(!is_better_quality(rate_2_1, rate_1_1));
    }

    #[test]
    fn quality_ordering() {
        let one = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let half = IOUAmount::new(5_000_000_000_000_000, -16).unwrap(); // 0.5
        let two = IOUAmount::new(2_000_000_000_000_000, -15).unwrap();

        // 0.5 / 1.0 = 0.5 (best quality for taker)
        let q_half = get_rate(&half, &one).unwrap();
        // 1.0 / 1.0 = 1.0
        let q_one = get_rate(&one, &one).unwrap();
        // 2.0 / 1.0 = 2.0 (worst quality for taker)
        let q_two = get_rate(&two, &one).unwrap();

        assert!(q_half < q_one);
        assert!(q_one < q_two);
    }

    #[test]
    fn decode_zero_rate() {
        let decoded = from_rate(0).unwrap();
        assert!(decoded.is_zero());
    }

    #[test]
    fn rate_packing_format() {
        let one = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let rate = get_rate(&one, &one).unwrap();

        // rate = 1.0 = 1000000000000000 * 10^-15
        // Packed exponent = -15 + 100 = 85
        let packed_exp = rate >> 56;
        let packed_mantissa = rate & 0x00FF_FFFF_FFFF_FFFF;
        assert_eq!(packed_exp, 85);
        assert_eq!(packed_mantissa, MIN_MANTISSA);
    }

    // --- Edge case tests ---

    #[test]
    fn rate_very_small_values() {
        // Very small rate: tiny_in / large_out -> underflows to zero rate
        let tiny = IOUAmount::from_parts(MIN_MANTISSA, -96, false).unwrap();
        let large = IOUAmount::from_parts(MIN_MANTISSA, 10, false).unwrap();
        let rate = get_rate(&tiny, &large).unwrap();
        // exp = -96 - 10 - 17 = -123, which underflows past MIN_EXPONENT to zero
        assert_eq!(rate, 0);
    }

    #[test]
    fn rate_small_but_representable() {
        // A small but representable rate
        let small = IOUAmount::from_parts(MIN_MANTISSA, -50, false).unwrap();
        let large = IOUAmount::from_parts(MIN_MANTISSA, 0, false).unwrap();
        let rate = get_rate(&small, &large).unwrap();
        assert_ne!(rate, 0);
        let decoded = from_rate(rate).unwrap();
        assert!(!decoded.is_zero());
    }

    #[test]
    fn rate_very_large_values() {
        // Very large rate: large_in / tiny_out
        let large = IOUAmount::from_parts(MIN_MANTISSA, 10, false).unwrap();
        let tiny = IOUAmount::from_parts(MIN_MANTISSA, -10, false).unwrap();
        let rate = get_rate(&large, &tiny).unwrap();
        assert_ne!(rate, 0);
        let decoded = from_rate(rate).unwrap();
        assert!(!decoded.is_zero());
    }

    #[test]
    fn rate_equal_amounts() {
        // Rate of equal amounts should be 1.0
        let a = IOUAmount::from_parts(5_000_000_000_000_000, 5, false).unwrap();
        let rate = get_rate(&a, &a).unwrap();
        let decoded = from_rate(rate).unwrap();
        assert_eq!(decoded.mantissa(), MIN_MANTISSA);
        assert_eq!(decoded.exponent(), -15);
    }

    #[test]
    fn better_quality_zero_handling() {
        // Zero quality is never better than anything
        assert!(!is_better_quality(0, 0));
        assert!(!is_better_quality(0, 100));
        assert!(is_better_quality(100, 0));
    }

    #[test]
    fn offer_quality_delegates_to_get_rate() {
        let a = IOUAmount::new(3_000_000_000_000_000, -15).unwrap();
        let b = IOUAmount::new(1_000_000_000_000_000, -15).unwrap();
        let q = offer_quality(&a, &b).unwrap();
        let r = get_rate(&a, &b).unwrap();
        assert_eq!(q, r);
    }
}
