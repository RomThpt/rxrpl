/// AMM helper functions for the constant-product market maker.
use rxrpl_primitives::AccountId;
use rxrpl_protocol::TransactionResult;
use serde_json::Value;

/// Convert an Asset JSON value to (currency_bytes, issuer_bytes).
///
/// XRP is represented as the string `"XRP"` and yields 20 zero bytes for both
/// currency and issuer. An IOU is an object with `currency` and `issuer`
/// fields.
pub fn asset_to_bytes(asset: &Value) -> Result<([u8; 20], [u8; 20]), TransactionResult> {
    if let Some(s) = asset.as_str() {
        if s == "XRP" {
            return Ok(([0u8; 20], [0u8; 20]));
        }
        return Err(TransactionResult::TemMalformed);
    }
    if asset.is_object() {
        let cur = asset
            .get("currency")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadCurrency)?;
        // {currency: "XRP"} is the canonical object form for native XRP and
        // doesn't carry an issuer.
        if cur == "XRP" {
            return Ok(([0u8; 20], [0u8; 20]));
        }
        let iss = asset
            .get("issuer")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadIssuer)?;
        let cur_bytes = crate::helpers::currency_to_bytes(cur);
        let iss_id = rxrpl_codec::address::classic::decode_account_id(iss)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let mut iss_bytes = [0u8; 20];
        iss_bytes.copy_from_slice(iss_id.as_bytes());
        Ok((cur_bytes, iss_bytes))
    } else {
        Err(TransactionResult::TemMalformed)
    }
}

/// Compute the AMM keylet from the Asset and Asset2 fields of a transaction.
pub fn compute_amm_key(
    asset1: &Value,
    asset2: &Value,
) -> Result<rxrpl_primitives::Hash256, TransactionResult> {
    let (cur1, iss1) = asset_to_bytes(asset1)?;
    let (cur2, iss2) = asset_to_bytes(asset2)?;
    let iss1_id = AccountId::from(iss1);
    let iss2_id = AccountId::from(iss2);
    Ok(rxrpl_protocol::keylet::amm(
        &cur1, &iss1_id, &cur2, &iss2_id,
    ))
}

/// Compute the AMM keylet from the Asset and Asset2 fields of a transaction
/// JSON object, extracting the fields automatically.
pub fn compute_amm_key_from_tx(tx: &Value) -> Result<rxrpl_primitives::Hash256, TransactionResult> {
    let asset1 = tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
    let asset2 = tx.get("Asset2").ok_or(TransactionResult::TemMalformed)?;
    compute_amm_key(asset1, asset2)
}

/// Sort two assets canonically. Returns (cur_low, iss_low, cur_high, iss_high).
pub fn sort_assets(
    cur_a: &[u8; 20],
    iss_a: &[u8; 20],
    cur_b: &[u8; 20],
    iss_b: &[u8; 20],
) -> ([u8; 20], [u8; 20], [u8; 20], [u8; 20]) {
    let a = (cur_a.as_slice(), iss_a.as_slice());
    let b = (cur_b.as_slice(), iss_b.as_slice());
    if a <= b {
        (*cur_a, *iss_a, *cur_b, *iss_b)
    } else {
        (*cur_b, *iss_b, *cur_a, *iss_a)
    }
}

/// Compute initial LP tokens for a new pool.
///
/// Uses the geometric-mean approximation: `min(amount1, amount2)`.
pub fn compute_lp_tokens_initial(amount1: u64, amount2: u64) -> u64 {
    amount1.min(amount2)
}

/// Compute LP tokens for a proportional deposit.
///
/// `tokens = deposit_a1 * total_lp / pool_a1`
///
/// A proportional deposit means `deposit_a1 / pool_a1 == deposit_a2 / pool_a2`.
pub fn compute_lp_tokens_deposit(
    pool_a1: u64,
    _pool_a2: u64,
    deposit_a1: u64,
    _deposit_a2: u64,
    total_lp: u64,
) -> u64 {
    if pool_a1 == 0 || total_lp == 0 {
        return 0;
    }
    ((deposit_a1 as u128) * (total_lp as u128) / (pool_a1 as u128)) as u64
}

/// Compute withdrawal amounts for burning LP tokens.
///
/// `amount_i = lp_burned * pool_i / total_lp`
pub fn compute_withdraw_amounts(
    pool_a1: u64,
    pool_a2: u64,
    lp_burned: u64,
    total_lp: u64,
) -> (u64, u64) {
    if total_lp == 0 {
        return (0, 0);
    }
    let out1 = ((lp_burned as u128) * (pool_a1 as u128) / (total_lp as u128)) as u64;
    let out2 = ((lp_burned as u128) * (pool_a2 as u128) / (total_lp as u128)) as u64;
    (out1, out2)
}

/// Validate that an Asset field is well-formed (XRP string or IOU object).
pub fn validate_asset(asset: &Value) -> Result<(), TransactionResult> {
    if let Some(s) = asset.as_str() {
        if s == "XRP" {
            return Ok(());
        }
        return Err(TransactionResult::TemMalformed);
    }
    if asset.is_object() {
        let cur = asset
            .get("currency")
            .and_then(|v| v.as_str())
            .filter(|c| !c.is_empty())
            .ok_or(TransactionResult::TemBadCurrency)?;
        // `{currency: "XRP"}` is the canonical XRP object form; no issuer.
        if cur == "XRP" {
            return Ok(());
        }
        asset
            .get("issuer")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemBadIssuer)?;
        return Ok(());
    }
    Err(TransactionResult::TemMalformed)
}

/// Check that two asset values are not equal. Treats `"XRP"` and
/// `{currency:"XRP"}` as the same asset (both are native XRP).
pub fn assets_differ(a: &Value, b: &Value) -> bool {
    let (cur_a, iss_a) = canonical_asset(a);
    let (cur_b, iss_b) = canonical_asset(b);
    cur_a != cur_b || iss_a != iss_b
}

fn canonical_asset(v: &Value) -> (Option<&str>, Option<&str>) {
    if let Some(s) = v.as_str() {
        return (Some(s), None);
    }
    let cur = v.get("currency").and_then(|c| c.as_str());
    if cur == Some("XRP") {
        return (Some("XRP"), None);
    }
    let iss = v.get("issuer").and_then(|i| i.as_str());
    (cur, iss)
}

/// Read an AMM entry from the view by its key.
pub fn read_amm(
    view: &dyn crate::view::read_view::ReadView,
    amm_key: &rxrpl_primitives::Hash256,
) -> Result<Value, TransactionResult> {
    let bytes = view.read(amm_key).ok_or(TransactionResult::TecNoEntry)?;
    serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)
}

/// Get a u64 field stored as a string from a JSON value.
pub fn get_pool_field(obj: &Value, field: &str) -> u64 {
    obj[field]
        .as_str()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Derive an Asset spec from an Amount field.
///
/// XRP string `"<drops>"` → `"XRP"` sentinel; IOU object `{currency, issuer,
/// value}` → `{currency, issuer}` extracted (drops the `value`). Returns
/// `None` on shapes the AMM can't represent (e.g. missing currency/issuer).
pub fn asset_spec_from_amount(amount: &Value) -> Option<Value> {
    if amount.is_string() {
        return Some(Value::String("XRP".to_string()));
    }
    let obj = amount.as_object()?;
    Some(serde_json::json!({
        "currency": obj.get("currency")?.clone(),
        "issuer": obj.get("issuer")?.clone(),
    }))
}

/// Parse an Amount field as an integer scalar, accepting both XRP strings
/// (`"<drops>"`) and IOU objects (`{value: "<numeric>"}`). Truncates IOU
/// fractional values; rejects negatives.
pub fn amount_value_drops_or_iou(amount: &Value) -> Option<u64> {
    if let Some(s) = amount.as_str() {
        return s.parse().ok();
    }
    let obj = amount.as_object()?;
    let v: f64 = obj.get("value")?.as_str()?.parse().ok()?;
    if v < 0.0 {
        return None;
    }
    Some(v as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xrp_asset_to_bytes() {
        let asset = serde_json::json!("XRP");
        let (cur, iss) = asset_to_bytes(&asset).unwrap();
        assert_eq!(cur, [0u8; 20]);
        assert_eq!(iss, [0u8; 20]);
    }

    #[test]
    fn iou_asset_to_bytes() {
        let asset = serde_json::json!({
            "currency": "USD",
            "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"
        });
        let (cur, iss) = asset_to_bytes(&asset).unwrap();
        // currency_to_bytes("USD") places bytes at offsets 12..15
        assert_eq!(cur[12], b'U');
        assert_eq!(cur[13], b'S');
        assert_eq!(cur[14], b'D');
        assert_ne!(iss, [0u8; 20]);
    }

    #[test]
    fn invalid_asset_string() {
        let asset = serde_json::json!("BTC");
        assert_eq!(asset_to_bytes(&asset), Err(TransactionResult::TemMalformed));
    }

    #[test]
    fn lp_tokens_initial() {
        assert_eq!(compute_lp_tokens_initial(100, 200), 100);
        assert_eq!(compute_lp_tokens_initial(500, 300), 300);
    }

    #[test]
    fn lp_tokens_deposit() {
        // Pool has 1000/2000, deposit 100/200, total_lp=1000
        // tokens = 100 * 1000 / 1000 = 100
        assert_eq!(compute_lp_tokens_deposit(1000, 2000, 100, 200, 1000), 100);
    }

    #[test]
    fn withdraw_amounts() {
        // Pool has 1000/2000, burn 500 of 1000 total LP
        let (out1, out2) = compute_withdraw_amounts(1000, 2000, 500, 1000);
        assert_eq!(out1, 500);
        assert_eq!(out2, 1000);
    }

    #[test]
    fn withdraw_from_empty_pool() {
        let (out1, out2) = compute_withdraw_amounts(0, 0, 100, 0);
        assert_eq!(out1, 0);
        assert_eq!(out2, 0);
    }

    #[test]
    fn assets_differ_xrp_vs_iou() {
        let a = serde_json::json!("XRP");
        let b = serde_json::json!({"currency": "USD", "issuer": "rFoo"});
        assert!(assets_differ(&a, &b));
    }

    #[test]
    fn assets_same_xrp() {
        let a = serde_json::json!("XRP");
        let b = serde_json::json!("XRP");
        assert!(!assets_differ(&a, &b));
    }

    #[test]
    fn amm_key_symmetric() {
        let a1 = serde_json::json!("XRP");
        let a2 =
            serde_json::json!({"currency": "USD", "issuer": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"});
        let k1 = compute_amm_key(&a1, &a2).unwrap();
        let k2 = compute_amm_key(&a2, &a1).unwrap();
        assert_eq!(k1, k2);
    }
}
