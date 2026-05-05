use rxrpl_amount::IOUAmount;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::AccountId;

use crate::line_cache::RippleLineCache;
use crate::types::{Issue, PATH_STEP_ACCOUNT, PATH_STEP_CURRENCY, PATH_STEP_ISSUER, PathStep};

/// Result of simulating a payment along a single strand (path).
#[derive(Debug, Clone)]
pub struct StrandResult {
    /// Amount that can actually be delivered through this path.
    pub delivered: IOUAmount,
    /// Amount of input required to deliver that output.
    pub input_required: IOUAmount,
    /// Quality ratio: delivered / input_required.
    /// Higher is better (more output per unit of input).
    pub quality: f64,
    /// Whether the path can deliver the full requested amount.
    pub fully_liquid: bool,
}

/// Offer extracted from the ledger for simulation purposes.
#[derive(Debug, Clone)]
struct BookOffer {
    taker_pays: IOUAmount,
    taker_gets: IOUAmount,
}

/// Simulate a payment along a strand to determine actual liquidity.
///
/// Walks each step of the path, computing how much can flow through
/// offer books and trust lines. Returns the quality and deliverable
/// amount for the strand.
pub fn simulate_strand(
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    path: &[PathStep],
    src_issue: &Issue,
    dst_issue: &Issue,
    source: &AccountId,
    destination: &AccountId,
    requested_amount: &IOUAmount,
) -> StrandResult {
    if path.is_empty() {
        return simulate_direct_path(
            ledger,
            line_cache,
            src_issue,
            dst_issue,
            source,
            destination,
            requested_amount,
        );
    }

    let steps: Vec<_> = path.iter().collect();

    // Forward pass: determine intermediate issues along the path.
    let mut step_issues = Vec::with_capacity(steps.len() + 1);
    step_issues.push(src_issue.clone());

    let mut tracking_issue = src_issue.clone();
    for step in &steps {
        tracking_issue = resolve_step_issue(step, &tracking_issue);
        step_issues.push(tracking_issue.clone());
    }

    // The final issue should match the destination issue.
    // If it does not, this path is invalid.
    let final_issue = step_issues.last().unwrap();
    if final_issue.currency != dst_issue.currency {
        return StrandResult {
            delivered: IOUAmount::ZERO,
            input_required: IOUAmount::ZERO,
            quality: 0.0,
            fully_liquid: false,
        };
    }

    // Forward simulation: push the requested amount through each step,
    // reducing it at each step based on available liquidity.
    let mut flow = *requested_amount;

    for (i, step) in steps.iter().enumerate() {
        let in_issue = &step_issues[i];
        let out_issue = &step_issues[i + 1];

        flow = simulate_step(ledger, line_cache, step, in_issue, out_issue, &flow);

        if flow.is_zero() {
            return StrandResult {
                delivered: IOUAmount::ZERO,
                input_required: IOUAmount::ZERO,
                quality: 0.0,
                fully_liquid: false,
            };
        }
    }

    let delivered = flow;

    // Reverse pass: determine how much input is needed for the delivered amount.
    let mut needed = delivered;
    for (i, step) in steps.iter().enumerate().rev() {
        let in_issue = &step_issues[i];
        let out_issue = &step_issues[i + 1];

        needed = compute_input_for_output(ledger, line_cache, step, in_issue, out_issue, &needed);

        if needed.is_zero() {
            return StrandResult {
                delivered: IOUAmount::ZERO,
                input_required: IOUAmount::ZERO,
                quality: 0.0,
                fully_liquid: false,
            };
        }
    }

    let total_input = needed;

    let quality = compute_quality(&delivered, &total_input);
    let fully_liquid = delivered >= *requested_amount;

    StrandResult {
        delivered,
        input_required: total_input,
        quality,
        fully_liquid,
    }
}

/// Simulate a direct path (no intermediate steps).
///
/// For same-currency paths this is a trust line transfer.
/// For XRP-to-XRP this is trivially fully liquid.
fn simulate_direct_path(
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    src_issue: &Issue,
    dst_issue: &Issue,
    source: &AccountId,
    destination: &AccountId,
    requested_amount: &IOUAmount,
) -> StrandResult {
    if src_issue.is_xrp() && dst_issue.is_xrp() {
        // XRP-to-XRP: always fully liquid (balance check is separate concern)
        return StrandResult {
            delivered: *requested_amount,
            input_required: *requested_amount,
            quality: 1.0,
            fully_liquid: true,
        };
    }

    if src_issue.currency == dst_issue.currency {
        // Same currency: check trust line balance
        let available =
            trust_line_available(ledger, line_cache, source, destination, &src_issue.currency);

        let delivered = min_amount(requested_amount, &available);
        let quality = if delivered.is_zero() {
            0.0
        } else {
            1.0 // Same currency, no exchange rate loss
        };

        return StrandResult {
            delivered,
            input_required: delivered,
            quality,
            fully_liquid: delivered >= *requested_amount,
        };
    }

    // Cross-currency direct path should not happen without steps
    StrandResult {
        delivered: IOUAmount::ZERO,
        input_required: IOUAmount::ZERO,
        quality: 0.0,
        fully_liquid: false,
    }
}

/// Determine the issue that a path step transitions to.
fn resolve_step_issue(step: &PathStep, current: &Issue) -> Issue {
    let currency = if (step.step_type & PATH_STEP_CURRENCY) != 0 {
        step.currency.unwrap_or(current.currency)
    } else {
        current.currency
    };

    let issuer = if (step.step_type & PATH_STEP_ISSUER) != 0 {
        step.issuer.unwrap_or(current.issuer)
    } else if (step.step_type & PATH_STEP_ACCOUNT) != 0 {
        step.account.unwrap_or(current.issuer)
    } else {
        current.issuer
    };

    Issue { currency, issuer }
}

/// Simulate a single step, returning how much output it can produce
/// given the input flow amount.
fn simulate_step(
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    step: &PathStep,
    in_issue: &Issue,
    out_issue: &Issue,
    input_flow: &IOUAmount,
) -> IOUAmount {
    if in_issue.currency == out_issue.currency && in_issue.issuer == out_issue.issuer {
        // Same issue: pass-through (rippling)
        return *input_flow;
    }

    if (step.step_type & PATH_STEP_ACCOUNT) != 0 {
        // Account step: trust line transfer through an intermediary
        if let Some(account) = &step.account {
            let available =
                trust_line_available_from(ledger, line_cache, account, &out_issue.currency);
            return min_amount(input_flow, &available);
        }
        return *input_flow;
    }

    // Currency/issuer step: offer book crossing
    let offers = collect_offers(ledger, in_issue, out_issue);
    if offers.is_empty() {
        return IOUAmount::ZERO;
    }

    consume_offers(&offers, input_flow)
}

/// Compute how much input is needed to produce the desired output at a step.
fn compute_input_for_output(
    ledger: &Ledger,
    _line_cache: &mut RippleLineCache,
    step: &PathStep,
    in_issue: &Issue,
    out_issue: &Issue,
    desired_output: &IOUAmount,
) -> IOUAmount {
    if in_issue.currency == out_issue.currency && in_issue.issuer == out_issue.issuer {
        // Same issue: 1:1
        return *desired_output;
    }

    if (step.step_type & PATH_STEP_ACCOUNT) != 0 {
        // Account step: trust line, 1:1 ratio
        return *desired_output;
    }

    // Offer book: compute input needed for output
    let offers = collect_offers(ledger, in_issue, out_issue);
    if offers.is_empty() {
        return IOUAmount::ZERO;
    }

    compute_input_from_offers(&offers, desired_output)
}

/// Check the available balance on a trust line between two accounts
/// for a given currency.
fn trust_line_available(
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    from: &AccountId,
    to: &AccountId,
    currency: &[u8; 20],
) -> IOUAmount {
    let lines = line_cache.get_lines(ledger, from);
    for line in lines {
        if line.peer == *to && line.currency == *currency {
            // Available = limit + balance (balance is from `from`'s perspective)
            let available = line.limit + line.balance;
            if available <= 0.0 {
                return IOUAmount::ZERO;
            }
            return f64_to_iou(available);
        }
    }
    // No trust line found: check reverse direction
    let lines = line_cache.get_lines(ledger, to);
    for line in lines {
        if line.peer == *from && line.currency == *currency {
            // From the peer's perspective, available outbound = peer_limit - balance
            let available = line.peer_limit - line.balance;
            if available <= 0.0 {
                return IOUAmount::ZERO;
            }
            return f64_to_iou(available);
        }
    }
    IOUAmount::ZERO
}

/// Check the total available outbound balance for an account in a currency
/// across all trust lines.
fn trust_line_available_from(
    ledger: &Ledger,
    line_cache: &mut RippleLineCache,
    account: &AccountId,
    currency: &[u8; 20],
) -> IOUAmount {
    let lines = line_cache.get_lines(ledger, account);
    let mut total = 0.0_f64;
    for line in lines {
        if line.currency == *currency {
            // Available on this line: how much `account` can send
            // balance is from account's perspective (positive = account is owed)
            // Can send up to: balance (if positive, i.e. peer owes us) + peer_limit
            let available = line.balance + line.peer_limit;
            if available > 0.0 {
                total += available;
            }
        }
    }
    if total <= 0.0 {
        return IOUAmount::ZERO;
    }
    f64_to_iou(total)
}

/// Collect offers from the ledger for a given trading pair.
///
/// Returns offers sorted by quality (best first).
fn collect_offers(ledger: &Ledger, pays_issue: &Issue, gets_issue: &Issue) -> Vec<BookOffer> {
    let mut offers = Vec::new();

    for (_key, data) in ledger.state_map.iter() {
        let entry: serde_json::Value = match serde_json::from_slice(&data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
            continue;
        }

        let taker_pays = match entry.get("TakerPays") {
            Some(v) => v,
            None => continue,
        };
        let taker_gets = match entry.get("TakerGets") {
            Some(v) => v,
            None => continue,
        };

        let offer_pays_issue = match parse_json_issue(taker_pays) {
            Some(i) => i,
            None => continue,
        };
        let offer_gets_issue = match parse_json_issue(taker_gets) {
            Some(i) => i,
            None => continue,
        };

        // Match: we want offers where taker pays pays_issue and gets gets_issue.
        // An offer on the book has TakerPays = what the offer owner wants (= our input)
        // and TakerGets = what the offer owner provides (= our output).
        // So: offer's TakerPays should match pays_issue (our input),
        // and offer's TakerGets should match gets_issue (our output).
        if offer_pays_issue.currency != pays_issue.currency
            || offer_gets_issue.currency != gets_issue.currency
        {
            continue;
        }

        // For IOU issues, also check issuer
        if !pays_issue.is_xrp() && offer_pays_issue.issuer != pays_issue.issuer {
            continue;
        }
        if !gets_issue.is_xrp() && offer_gets_issue.issuer != gets_issue.issuer {
            continue;
        }

        let pays_amount = parse_json_amount(taker_pays);
        let gets_amount = parse_json_amount(taker_gets);

        if let (Some(pays), Some(gets)) = (pays_amount, gets_amount) {
            if !pays.is_zero() && !gets.is_zero() {
                offers.push(BookOffer {
                    taker_pays: pays,
                    taker_gets: gets,
                });
            }
        }
    }

    // Sort by quality: taker_pays / taker_gets (lower is better for us)
    offers.sort_by(|a, b| {
        let qa = offer_quality_f64(&a.taker_pays, &a.taker_gets);
        let qb = offer_quality_f64(&b.taker_pays, &b.taker_gets);
        qa.partial_cmp(&qb).unwrap_or(std::cmp::Ordering::Equal)
    });

    offers
}

/// Consume offers with the given input amount.
///
/// Returns total output obtained by consuming offers in quality order.
fn consume_offers(offers: &[BookOffer], input: &IOUAmount) -> IOUAmount {
    let mut remaining_input = *input;
    let mut total_output = IOUAmount::ZERO;

    for offer in offers {
        if remaining_input.is_zero() {
            break;
        }

        // How much of this offer can we consume?
        // If our remaining input >= offer's taker_pays, consume the whole offer.
        // Otherwise, consume proportionally.
        if remaining_input >= offer.taker_pays {
            // Consume entire offer
            total_output = match IOUAmount::add(&total_output, &offer.taker_gets) {
                Ok(v) => v,
                Err(_) => break,
            };
            remaining_input = match IOUAmount::sub(&remaining_input, &offer.taker_pays) {
                Ok(v) => v,
                Err(_) => break,
            };
        } else {
            // Partial consumption: output = gets * (remaining / pays)
            let ratio = match IOUAmount::divide(&remaining_input, &offer.taker_pays) {
                Ok(v) => v,
                Err(_) => break,
            };
            let partial_output = match IOUAmount::multiply(&offer.taker_gets, &ratio) {
                Ok(v) => v,
                Err(_) => break,
            };
            total_output = match IOUAmount::add(&total_output, &partial_output) {
                Ok(v) => v,
                Err(_) => break,
            };
            remaining_input = IOUAmount::ZERO;
        }
    }

    total_output
}

/// Compute how much input is needed to obtain the desired output from offers.
fn compute_input_from_offers(offers: &[BookOffer], desired_output: &IOUAmount) -> IOUAmount {
    let mut remaining_output = *desired_output;
    let mut total_input = IOUAmount::ZERO;

    for offer in offers {
        if remaining_output.is_zero() {
            break;
        }

        if remaining_output >= offer.taker_gets {
            // Need entire offer
            total_input = match IOUAmount::add(&total_input, &offer.taker_pays) {
                Ok(v) => v,
                Err(_) => break,
            };
            remaining_output = match IOUAmount::sub(&remaining_output, &offer.taker_gets) {
                Ok(v) => v,
                Err(_) => break,
            };
        } else {
            // Partial: input = pays * (remaining / gets)
            let ratio = match IOUAmount::divide(&remaining_output, &offer.taker_gets) {
                Ok(v) => v,
                Err(_) => break,
            };
            let partial_input = match IOUAmount::multiply(&offer.taker_pays, &ratio) {
                Ok(v) => v,
                Err(_) => break,
            };
            total_input = match IOUAmount::add(&total_input, &partial_input) {
                Ok(v) => v,
                Err(_) => break,
            };
            remaining_output = IOUAmount::ZERO;
        }
    }

    // If we still need more output, the book is insufficient
    if !remaining_output.is_zero() {
        return IOUAmount::ZERO;
    }

    total_input
}

/// Compute quality ratio as f64: delivered / input.
/// Higher is better.
fn compute_quality(delivered: &IOUAmount, input: &IOUAmount) -> f64 {
    if input.is_zero() || delivered.is_zero() {
        return 0.0;
    }

    match IOUAmount::divide(delivered, input) {
        Ok(ratio) => iou_to_f64(&ratio),
        Err(_) => 0.0,
    }
}

/// Convert an IOUAmount to f64 for scoring purposes.
fn iou_to_f64(amount: &IOUAmount) -> f64 {
    if amount.is_zero() {
        return 0.0;
    }
    let sign = if amount.is_negative() { -1.0 } else { 1.0 };
    sign * (amount.mantissa() as f64) * 10.0_f64.powi(amount.exponent())
}

/// Convert an f64 to IOUAmount.
fn f64_to_iou(value: f64) -> IOUAmount {
    if value == 0.0 || !value.is_finite() {
        return IOUAmount::ZERO;
    }

    let negative = value < 0.0;
    let abs_val = value.abs();

    // Find appropriate mantissa and exponent
    let mut exponent = 0i32;
    let mut mantissa = abs_val;

    // Scale to get mantissa into [10^15, 10^16) range
    while mantissa < 1e15 && exponent > -96 {
        mantissa *= 10.0;
        exponent -= 1;
    }
    while mantissa >= 1e16 && exponent < 80 {
        mantissa /= 10.0;
        exponent += 1;
    }

    let m = mantissa as u64;
    if m == 0 {
        return IOUAmount::ZERO;
    }

    IOUAmount::from_parts(m, exponent, negative).unwrap_or(IOUAmount::ZERO)
}

/// Return the minimum of two IOUAmounts (both assumed non-negative).
fn min_amount(a: &IOUAmount, b: &IOUAmount) -> IOUAmount {
    if *a <= *b { *a } else { *b }
}

/// Compute offer quality as f64 for sorting.
fn offer_quality_f64(pays: &IOUAmount, gets: &IOUAmount) -> f64 {
    if gets.is_zero() {
        return f64::MAX;
    }
    match IOUAmount::divide(pays, gets) {
        Ok(ratio) => iou_to_f64(&ratio),
        Err(_) => f64::MAX,
    }
}

/// Parse a JSON amount value into an Issue.
fn parse_json_issue(amount: &serde_json::Value) -> Option<Issue> {
    if amount.is_string() {
        return Some(Issue::xrp());
    }

    let currency_str = amount.get("currency").and_then(|v| v.as_str())?;
    let issuer_str = amount.get("issuer").and_then(|v| v.as_str())?;

    let mut currency = [0u8; 20];
    if currency_str.len() == 3 {
        currency[12] = currency_str.as_bytes()[0];
        currency[13] = currency_str.as_bytes()[1];
        currency[14] = currency_str.as_bytes()[2];
    } else if currency_str.len() == 40 {
        let decoded = hex::decode(currency_str).ok()?;
        currency.copy_from_slice(&decoded);
    }

    let issuer = rxrpl_codec::address::classic::decode_account_id(issuer_str).ok()?;
    Some(Issue { currency, issuer })
}

/// Parse a JSON amount into an IOUAmount.
fn parse_json_amount(amount: &serde_json::Value) -> Option<IOUAmount> {
    if let Some(drops_str) = amount.as_str() {
        // XRP drops
        let drops: i64 = drops_str.parse().ok()?;
        if drops == 0 {
            return Some(IOUAmount::ZERO);
        }
        // Convert drops to a normalized IOUAmount
        return IOUAmount::new(drops, 0).ok();
    }

    let value_str = amount.get("value").and_then(|v| v.as_str())?;
    let value: f64 = value_str.parse().ok()?;
    Some(f64_to_iou(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_iou(value: f64) -> IOUAmount {
        f64_to_iou(value)
    }

    #[test]
    fn f64_iou_roundtrip() {
        let original = 100.0;
        let iou = f64_to_iou(original);
        let back = iou_to_f64(&iou);
        assert!((back - original).abs() < 0.01);
    }

    #[test]
    fn f64_to_iou_zero() {
        let iou = f64_to_iou(0.0);
        assert!(iou.is_zero());
    }

    #[test]
    fn min_amount_returns_smaller() {
        let a = make_iou(50.0);
        let b = make_iou(100.0);
        let result = min_amount(&a, &b);
        assert_eq!(result, a);
    }

    #[test]
    fn consume_offers_full() {
        // One offer: pays 100 USD, gets 200 EUR
        // Input 100 USD should yield 200 EUR
        let offers = vec![BookOffer {
            taker_pays: make_iou(100.0),
            taker_gets: make_iou(200.0),
        }];
        let input = make_iou(100.0);
        let output = consume_offers(&offers, &input);
        let output_f64 = iou_to_f64(&output);
        assert!((output_f64 - 200.0).abs() < 1.0);
    }

    #[test]
    fn consume_offers_partial() {
        // One offer: pays 100, gets 200
        // Input 50 should yield ~100
        let offers = vec![BookOffer {
            taker_pays: make_iou(100.0),
            taker_gets: make_iou(200.0),
        }];
        let input = make_iou(50.0);
        let output = consume_offers(&offers, &input);
        let output_f64 = iou_to_f64(&output);
        assert!((output_f64 - 100.0).abs() < 1.0);
    }

    #[test]
    fn consume_offers_multiple() {
        // Two offers: first pays 50 gets 100, second pays 50 gets 80
        // Input 100 should consume both: 100 + 80 = 180
        let offers = vec![
            BookOffer {
                taker_pays: make_iou(50.0),
                taker_gets: make_iou(100.0),
            },
            BookOffer {
                taker_pays: make_iou(50.0),
                taker_gets: make_iou(80.0),
            },
        ];
        let input = make_iou(100.0);
        let output = consume_offers(&offers, &input);
        let output_f64 = iou_to_f64(&output);
        assert!((output_f64 - 180.0).abs() < 1.0);
    }

    #[test]
    fn consume_offers_empty_book() {
        let offers: Vec<BookOffer> = vec![];
        let input = make_iou(100.0);
        let output = consume_offers(&offers, &input);
        assert!(output.is_zero());
    }

    #[test]
    fn compute_input_from_offers_full() {
        // Offer: pays 100, gets 200
        // Want 200 output, need 100 input
        let offers = vec![BookOffer {
            taker_pays: make_iou(100.0),
            taker_gets: make_iou(200.0),
        }];
        let desired = make_iou(200.0);
        let input = compute_input_from_offers(&offers, &desired);
        let input_f64 = iou_to_f64(&input);
        assert!((input_f64 - 100.0).abs() < 1.0);
    }

    #[test]
    fn compute_input_from_offers_partial_need() {
        // Offer: pays 100, gets 200
        // Want 100 output, need 50 input
        let offers = vec![BookOffer {
            taker_pays: make_iou(100.0),
            taker_gets: make_iou(200.0),
        }];
        let desired = make_iou(100.0);
        let input = compute_input_from_offers(&offers, &desired);
        let input_f64 = iou_to_f64(&input);
        assert!((input_f64 - 50.0).abs() < 1.0);
    }

    #[test]
    fn compute_input_insufficient_book() {
        // Offer: pays 50, gets 100
        // Want 200 output, only 100 available -> returns zero
        let offers = vec![BookOffer {
            taker_pays: make_iou(50.0),
            taker_gets: make_iou(100.0),
        }];
        let desired = make_iou(200.0);
        let input = compute_input_from_offers(&offers, &desired);
        assert!(input.is_zero());
    }

    #[test]
    fn quality_computation() {
        let delivered = make_iou(95.0);
        let input = make_iou(100.0);
        let q = compute_quality(&delivered, &input);
        assert!((q - 0.95).abs() < 0.01);
    }

    #[test]
    fn quality_zero_input() {
        let delivered = make_iou(100.0);
        let input = IOUAmount::ZERO;
        let q = compute_quality(&delivered, &input);
        assert_eq!(q, 0.0);
    }

    #[test]
    fn quality_zero_delivered() {
        let delivered = IOUAmount::ZERO;
        let input = make_iou(100.0);
        let q = compute_quality(&delivered, &input);
        assert_eq!(q, 0.0);
    }

    #[test]
    fn resolve_step_currency_change() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let mut eur = [0u8; 20];
        eur[12..15].copy_from_slice(b"EUR");
        let issuer = AccountId([1u8; 20]);

        let current = Issue {
            currency: usd,
            issuer,
        };

        let step = PathStep {
            account: None,
            currency: Some(eur),
            issuer: Some(issuer),
            step_type: PATH_STEP_CURRENCY | PATH_STEP_ISSUER,
        };

        let result = resolve_step_issue(&step, &current);
        assert_eq!(result.currency, eur);
        assert_eq!(result.issuer, issuer);
    }

    #[test]
    fn resolve_step_account() {
        let mut usd = [0u8; 20];
        usd[12..15].copy_from_slice(b"USD");
        let issuer1 = AccountId([1u8; 20]);
        let issuer2 = AccountId([2u8; 20]);

        let current = Issue {
            currency: usd,
            issuer: issuer1,
        };

        let step = PathStep {
            account: Some(issuer2),
            currency: None,
            issuer: None,
            step_type: PATH_STEP_ACCOUNT,
        };

        let result = resolve_step_issue(&step, &current);
        assert_eq!(result.currency, usd); // Currency unchanged
        assert_eq!(result.issuer, issuer2); // Issuer updated to account
    }

    #[test]
    fn direct_xrp_path_fully_liquid() {
        let ledger = rxrpl_ledger::Ledger::genesis();
        let mut line_cache = RippleLineCache::new();
        let source = AccountId([1u8; 20]);
        let dest = AccountId([2u8; 20]);
        let amount = make_iou(1000.0);

        let result = simulate_direct_path(
            &ledger,
            &mut line_cache,
            &Issue::xrp(),
            &Issue::xrp(),
            &source,
            &dest,
            &amount,
        );

        assert!(result.fully_liquid);
        assert_eq!(result.quality, 1.0);
    }

    #[test]
    fn parse_json_amount_xrp_drops() {
        let amount = serde_json::json!("1000000");
        let iou = parse_json_amount(&amount).unwrap();
        assert!(!iou.is_zero());
    }

    #[test]
    fn parse_json_amount_iou() {
        let amount = serde_json::json!({
            "currency": "USD",
            "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "value": "100",
        });
        let iou = parse_json_amount(&amount).unwrap();
        let val = iou_to_f64(&iou);
        assert!((val - 100.0).abs() < 0.01);
    }
}
