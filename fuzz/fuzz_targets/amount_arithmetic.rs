#![no_main]
use libfuzzer_sys::fuzz_target;

use arbitrary::Arbitrary;
use rxrpl_amount::IOUAmount;

#[derive(Arbitrary, Debug)]
struct AmountInput {
    mantissa_a: i64,
    exponent_a: i8,
    mantissa_b: i64,
    exponent_b: i8,
    numerator: u32,
    denominator: u32,
    round_up: bool,
}

fuzz_target!(|input: AmountInput| {
    // Clamp exponents to valid-ish range to get more interesting inputs
    let exp_a = (input.exponent_a as i32).clamp(-100, 85);
    let exp_b = (input.exponent_b as i32).clamp(-100, 85);

    let a = IOUAmount::new(input.mantissa_a, exp_a);
    let b = IOUAmount::new(input.mantissa_b, exp_b);

    // Exercise all arithmetic operations, accepting any error gracefully
    if let (Ok(a), Ok(b)) = (&a, &b) {
        // Addition and subtraction
        let _ = IOUAmount::add(a, b);
        let _ = IOUAmount::sub(a, b);
        let _ = IOUAmount::add(b, a);
        let _ = IOUAmount::sub(b, a);

        // Multiplication
        let _ = IOUAmount::multiply(a, b);
        let _ = IOUAmount::mul_round(a, b, input.round_up);
        let _ = IOUAmount::mul_round(a, b, !input.round_up);

        // Division
        let _ = IOUAmount::divide(a, b);
        let _ = IOUAmount::divide(b, a);
        let _ = IOUAmount::div_round(a, b, input.round_up);
        let _ = IOUAmount::div_round(b, a, input.round_up);

        // mul_ratio
        let _ = a.mul_ratio(input.numerator, input.denominator, input.round_up);
        let _ = b.mul_ratio(input.numerator, input.denominator, !input.round_up);

        // Negate and abs
        let neg_a = a.negate();
        let abs_a = a.abs();
        let _ = neg_a.is_zero();
        let _ = abs_a.is_negative();

        // Ordering
        let _ = a.cmp(b);
        let _ = a == b;

        // Self-operations
        let _ = IOUAmount::multiply(a, a);
        let _ = IOUAmount::add(a, &a.negate());

        // Display
        let _ = a.to_string();
        let _ = b.to_string();
    }

    // Also fuzz from_parts with unsigned mantissa
    let _ = IOUAmount::from_parts(input.mantissa_a as u64, exp_a, input.round_up);
    let _ = IOUAmount::from_parts(input.mantissa_b as u64, exp_b, false);

    // Edge cases: zero operations
    if let Ok(a) = &a {
        let _ = IOUAmount::add(a, &IOUAmount::ZERO);
        let _ = IOUAmount::multiply(a, &IOUAmount::ZERO);
        let _ = IOUAmount::divide(&IOUAmount::ZERO, a);
    }
});
