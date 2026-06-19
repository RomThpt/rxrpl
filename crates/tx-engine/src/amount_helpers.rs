//! Shared helpers for IOU (RippleState) balances.
//!
//! `RippleState.Balance.value` is stored from the low-account perspective;
//! helpers here translate to/from the holder-centric view used by handlers.

use rxrpl_amount::IOUAmount;
use rxrpl_primitives::AccountId;
use rxrpl_protocol::TransactionResult;

/// Compute the holder's IOU balance against the issuer (always non-negative
/// from the holder's perspective). Returns `0.0` when the holder owes the issuer.
pub fn compute_holder_balance(
    trust: &serde_json::Value,
    issuer_id: &AccountId,
    holder_id: &AccountId,
) -> f64 {
    let raw: f64 = trust
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .parse()
        .unwrap_or(0.0);
    let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
    let holder_view = if holder_is_low { raw } else { -raw };
    holder_view.max(0.0)
}

/// Compute the post-update `Balance.value` after applying `delta_str` to the
/// holder's IOU balance (positive = credit, negative = debit), as an exact
/// decimal string. Uses rippled-exact `IOUAmount` round-to-nearest arithmetic;
/// an f64 path loses the last mantissa digit on high-precision balances. (Safe
/// across eras: pre-2014 IOU balances are low-precision, where round and the
/// legacy truncation agree.)
pub fn compute_new_iou_balance(
    trust: &serde_json::Value,
    delta_str: &str,
    issuer_id: &AccountId,
    holder_id: &AccountId,
) -> Result<String, TransactionResult> {
    let cur_str = trust
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0");
    let current =
        IOUAmount::from_decimal_string(cur_str).map_err(|_| TransactionResult::TefInternal)?;
    let delta =
        IOUAmount::from_decimal_string(delta_str).map_err(|_| TransactionResult::TemBadAmount)?;
    let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
    let signed_delta = if holder_is_low { delta } else { delta.negate() };
    let result = IOUAmount::add_round(&current, &signed_delta)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(result.to_decimal_string())
}
