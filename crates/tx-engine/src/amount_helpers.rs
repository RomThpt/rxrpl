//! Shared helpers for IOU (RippleState) balances.
//!
//! `RippleState.Balance.value` is stored from the low-account perspective;
//! helpers here translate to/from the holder-centric view used by handlers.

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

/// Compute the post-update raw `Balance.value` after applying `delta_str`
/// to the holder's IOU balance (positive = credit, negative = debit).
///
/// Returns the raw `f64`; callers format the string with their own
/// convention (Payment uses `format_iou_value`; check_cash trims a 15-digit
/// representation).
pub fn compute_new_iou_balance(
    trust: &serde_json::Value,
    delta_str: &str,
    issuer_id: &AccountId,
    holder_id: &AccountId,
) -> Result<f64, TransactionResult> {
    let current: f64 = trust
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .parse()
        .map_err(|_| TransactionResult::TefInternal)?;
    let delta: f64 = delta_str
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
    Ok(if holder_is_low {
        current + delta
    } else {
        current - delta
    })
}
