/// AMM helper functions for the constant-product market maker.
use rxrpl_codec::address::classic::encode_account_id;
use rxrpl_primitives::{AccountId, Hash256};
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::owner_dir::add_to_owner_dir;
use crate::view::apply_view::ApplyView;
use crate::view::read_view::ReadView;

/// Fee multipliers `(f1, f2)` mirroring rippled `feeMult`/`feeMultHalf`:
/// `getFee = tfee/100000`, `f1 = 1 - getFee`, `f2 = (1 - getFee/2) / f1`. The
/// computation order matches rippled so the `Number` rounding is byte-faithful.
fn fee_mults(tfee: u16) -> (rxrpl_amount::number::Number, rxrpl_amount::number::Number) {
    use rxrpl_amount::number::Number;
    let one = Number::from_int(1);
    let two = Number::from_int(2);
    let fee = Number::from_int(tfee as i64).div(&Number::from_int(100_000));
    let f1 = one.sub(&fee);
    let f2 = one.sub(&fee.div(&two)).div(&f1);
    (f1, f2)
}

/// Single-asset deposit: LP tokens issued for depositing `deposit` of an asset
/// whose pool balance is `pool`, against `total_lp` outstanding, with trading
/// fee `tfee` (1/100000). Mirrors rippled `lpTokensOut` under fixAMMv1_3
/// (rounds the issued tokens down).
pub fn lp_tokens_out_single(
    pool: u64,
    deposit: u64,
    total_lp: &rxrpl_amount::IOUAmount,
    tfee: u16,
) -> rxrpl_amount::IOUAmount {
    use rxrpl_amount::number::Number;
    let r = Number::from_int(deposit as i64).div(&Number::from_int(pool as i64));
    lp_tokens_out_ratio(&r, total_lp, tfee)
}

/// Single-asset deposit LP tokens given the deposit/pool ratio `r` (works for
/// XRP or IOU legs). Mirrors rippled `lpTokensOut` + `adjustLPTokens`.
pub fn lp_tokens_out_ratio(
    r: &rxrpl_amount::number::Number,
    total_lp: &rxrpl_amount::IOUAmount,
    tfee: u16,
) -> rxrpl_amount::IOUAmount {
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode, root2};
    let one = Number::from_int(1);
    let (f1, f2) = fee_mults(tfee);
    let c = root2(f2.mul(&f2).add(&r.div(&f1))).sub(&f2);
    let frac = r.sub(&c).div(&one.add(&c));
    let t = Number::from_iou(total_lp);

    let _g = RoundModeGuard::new(RoundingMode::Downward);
    let raw = t.mul(&frac).to_iou();
    let t_plus = t.add(&Number::from_iou(&raw)).to_iou();
    Number::from_iou(&t_plus).sub(&t).to_iou()
}

/// Single-asset withdraw: LP tokens burned to withdraw `withdraw` of an asset
/// with pool balance `pool`. Mirrors rippled `lpTokensIn` (eq 7) under
/// fixAMMv1_3 (rounds tokens up), then `adjustLPTokens` (withdraw variant).
pub fn lp_tokens_in_single(
    pool: u64,
    withdraw: u64,
    total_lp: &rxrpl_amount::IOUAmount,
    tfee: u16,
) -> rxrpl_amount::IOUAmount {
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode, root2};
    // fr, c and frac are computed under the default (to-nearest) mode; only the
    // final multiply that mints tokens rounds upward.
    let two = Number::from_int(2);
    let fr = Number::from_int(withdraw as i64).div(&Number::from_int(pool as i64));
    let f1 = Number::from_int(tfee as i64).div(&Number::from_int(100_000)); // getFee
    let c = fr.mul(&f1).add(&two).sub(&f1);
    let disc = root2(c.mul(&c).sub(&Number::from_int(4).mul(&fr)));
    let frac = c.sub(&disc).div(&two);
    let t = Number::from_iou(total_lp);

    let raw = {
        let _g = RoundModeGuard::new(RoundingMode::Upward);
        t.mul(&frac).to_iou()
    };
    // adjustLPTokens (withdraw): (raw - T) + T under downward rounding.
    let _g = RoundModeGuard::new(RoundingMode::Downward);
    let minus = Number::from_iou(&raw).sub(&t).to_iou();
    Number::from_iou(&minus).add(&t).to_iou()
}

/// `ammAssetOut` (eq 8): XRP drops paid out for burning `tokens` LP, minimised
/// (rounded down).
pub fn amm_asset_out_single_xrp(
    pool: u64,
    total_lp: &rxrpl_amount::IOUAmount,
    tokens: &rxrpl_amount::IOUAmount,
    tfee: u16,
) -> u64 {
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode};
    let t1 = Number::from_iou(tokens).div(&Number::from_iou(total_lp));
    let f = Number::from_int(tfee as i64).div(&Number::from_int(100_000));
    let two = Number::from_int(2);
    let num = t1.mul(&t1).sub(&t1.mul(&two.sub(&f)));
    let den = t1.mul(&f).sub(&Number::from_int(1));
    let frac = num.div(&den);
    let _g = RoundModeGuard::new(RoundingMode::Downward);
    Number::from_int(pool as i64).mul(&frac).to_xrp_drops()
}

/// `ammAssetIn` (eq 4): the asset amount required to mint `tokens` LP, given a
/// pool balance `pool` and outstanding `total_lp`. Mirrors rippled (maximize
/// deposit → rounded up). Used by `adjustAssetInByTokens` to re-derive the
/// actual deposit consistent with the issued tokens.
pub fn amm_asset_in(
    pool: &rxrpl_amount::number::Number,
    total_lp: &rxrpl_amount::IOUAmount,
    tokens: &rxrpl_amount::IOUAmount,
    tfee: u16,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::{Number, RoundModeGuard, RoundingMode, root2};
    let one = Number::from_int(1);
    let (f1, f2) = fee_mults(tfee);
    let t1 = Number::from_iou(tokens).div(&Number::from_iou(total_lp));
    let t2 = one.add(&t1);
    let d = f2.sub(&t1.div(&t2));
    let a = one.div(&t2.mul(&t2));
    let two = Number::from_int(2);
    let b = two.mul(&d).div(&t2).sub(&one.div(&f1));
    let c = d.mul(&d).sub(&f2.mul(&f2));
    // solveQuadraticEq: (-b + root2(b*b - 4*a*c)) / (2*a)
    let disc = root2(b.mul(&b).sub(&Number::from_int(4).mul(&a).mul(&c)));
    let frac = b.negate().add(&disc).div(&two.mul(&a));
    // rippled `ammAssetIn` returns `multiply(balance, frac, Upward)` =
    // toSTAmount(asset, balance*frac, Upward): the result lands on the IOU
    // grid rounded up, not at full Number precision.
    let _g = RoundModeGuard::new(RoundingMode::Upward);
    Number::from_iou(&pool.mul(&frac).to_iou())
}

/// Parse an IOU `value` decimal string into an `IOUAmount`.
pub fn parse_iou_value(s: &str) -> rxrpl_amount::IOUAmount {
    rxrpl_amount::IOUAmount::from_decimal_string(s).unwrap_or(rxrpl_amount::IOUAmount::ZERO)
}

/// The account's signed holding of an IOU (its own perspective) as a `Number`,
/// read from the `account`↔`issuer` trust line. A `RippleState` stores the
/// balance from the low account's perspective, so the holding is the stored
/// balance when the account is low and its negation when it is high.
pub fn iou_holding_number(
    view: &dyn ReadView,
    account: &AccountId,
    issuer: &AccountId,
    currency: &[u8; 20],
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    let tl_key = keylet::trust_line(account, issuer, currency);
    let Some(bytes) = view.read(&tl_key) else {
        return Number::ZERO;
    };
    let Ok(line): Result<Value, _> = serde_json::from_slice(&bytes) else {
        return Number::ZERO;
    };
    let bal_str = line["Balance"]["value"].as_str().unwrap_or("0");
    let neg = bal_str.starts_with('-');
    let mag = parse_iou_value(bal_str.trim_start_matches('-'));
    let stored = if neg {
        Number::from_iou(&mag).negate()
    } else {
        Number::from_iou(&mag)
    };
    if account.as_bytes() < issuer.as_bytes() {
        stored
    } else {
        stored.negate()
    }
}

/// Write the account's new holding of an IOU back to its trust line, restoring
/// the low-account-perspective sign convention.
pub fn set_iou_holding(
    view: &mut dyn ApplyView,
    account: &AccountId,
    issuer: &AccountId,
    currency: &[u8; 20],
    new_holding: &rxrpl_amount::number::Number,
) -> Result<(), TransactionResult> {
    let tl_key = keylet::trust_line(account, issuer, currency);
    let bytes = view.read(&tl_key).ok_or(TransactionResult::TecNoEntry)?;
    let mut line: Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let stored = if account.as_bytes() < issuer.as_bytes() {
        *new_holding
    } else {
        new_holding.negate()
    };
    line["Balance"]["value"] = Value::String(stored.to_iou().to_decimal_string());
    let data = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
    view.update(tl_key, data)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

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

// ---------------------------------------------------------------------------
// LP-token helpers
//
// rippled mints LP tokens by issuing a per-AMM IOU from a deterministic
// AMM "pseudo-account" (derived from the AMM keylet) and tracking each
// holder's balance on a `RippleState` trust line. account_lines / account
// _objects / amm_info all read these.
//
// Conventions used here (must agree with `amm_info`):
//   - pseudo-account = first 20 bytes of the AMM keylet hash
//   - LP currency    = 0x03 || amm_key[12..31] (20 bytes; rendered as 40-char hex)
//
// This isn't byte-for-byte rippled (rippled chains a hash to derive both),
// but it's stable per-AMM and matches what `amm_info` already returns, so
// account_lines lookups by `peer = LPTokenIssuer` resolve to the same line
// xrpl-hive expects.
// ---------------------------------------------------------------------------

/// Derive the AMM pseudo-account from its keylet (first 20 bytes).
pub fn amm_pseudo_account(amm_key: &Hash256) -> AccountId {
    let mut bytes = [0u8; 20];
    bytes.copy_from_slice(&amm_key.as_bytes()[..20]);
    AccountId::new(bytes)
}

/// Derive the 20-byte LP currency code from the AMM keylet.
pub fn lp_currency_bytes(amm_key: &Hash256) -> [u8; 20] {
    let mut cur = [0u8; 20];
    cur[0] = 0x03;
    cur[1..].copy_from_slice(&amm_key.as_bytes()[12..31]);
    cur
}

/// Uppercase-hex string form of the LP currency.
pub fn lp_currency_hex(amm_key: &Hash256) -> String {
    hex::encode_upper(lp_currency_bytes(amm_key))
}

/// Read the holder's current LP balance (0 if no line exists).
pub fn lp_balance_of(view: &dyn ReadView, amm_key: &Hash256, holder: &AccountId) -> u64 {
    let pseudo = amm_pseudo_account(amm_key);
    let cur = lp_currency_bytes(amm_key);
    let tl_key = keylet::trust_line(holder, &pseudo, &cur);
    let Some(bytes) = view.read(&tl_key) else {
        return 0;
    };
    let Ok(line): Result<Value, _> = serde_json::from_slice(&bytes) else {
        return 0;
    };
    line.get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches('-'))
        .and_then(|s| s.parse::<f64>().ok())
        .map(|f| f as u64)
        .unwrap_or(0)
}

/// Mint (or burn, with `delta < 0`) LP tokens for `holder`. Creates the
/// RippleState line if missing and links it into the holder's owner
/// directory. Returns the new balance.
///
/// The line is stored from the holder's perspective: the holder's balance
/// is positive (asset they hold), the AMM pseudo-account is the issuer.
/// Balance currency/issuer match what `amm_info` reports as `LPToken`.
pub fn adjust_lp_balance(
    view: &mut dyn ApplyView,
    amm_key: &Hash256,
    holder: &AccountId,
    delta: i128,
) -> Result<u64, TransactionResult> {
    let pseudo = amm_pseudo_account(amm_key);
    let cur_bytes = lp_currency_bytes(amm_key);
    let cur_hex = hex::encode_upper(cur_bytes);
    let tl_key = keylet::trust_line(holder, &pseudo, &cur_bytes);
    let issuer_str = encode_account_id(&pseudo);
    let holder_str = encode_account_id(holder);

    // Layout: low/high follow account-id ordering (matches TrustSet).
    // The holder's balance is stored signed: positive when low_account ==
    // holder, negative when high_account == holder. account_lines flips
    // the sign back when rendering, so we follow the same convention.
    let holder_is_low = holder.as_bytes() < pseudo.as_bytes();

    let (existing_balance_signed, mut line, exists): (i128, Value, bool) = match view.read(&tl_key)
    {
        Some(b) => {
            let v: Value =
                serde_json::from_slice(&b).map_err(|_| TransactionResult::TefInternal)?;
            let raw = v
                .get("Balance")
                .and_then(|b| b.get("value"))
                .and_then(|s| s.as_str())
                .unwrap_or("0");
            let signed: i128 = raw.parse::<f64>().map(|f| f as i128).unwrap_or(0);
            (signed, v, true)
        }
        None => (0, Value::Null, false),
    };

    // Unsigned holder-side balance (always >= 0).
    let holder_balance: i128 = if holder_is_low {
        existing_balance_signed
    } else {
        -existing_balance_signed
    };
    let new_holder_balance = holder_balance.saturating_add(delta).max(0);

    // Storage form: positive when holder is low, negative when high.
    let new_signed = if holder_is_low {
        new_holder_balance
    } else {
        -new_holder_balance
    };
    let balance_str = new_signed.to_string();

    if !exists {
        // Limit/issuer pair — both sides reference the LP currency. The
        // pseudo-account "high" side carries a zero limit (it's the issuer);
        // the holder side carries a permissive limit so account_lines
        // reports the line regardless of which side is queried.
        let holder_limit = serde_json::json!({
            "currency": cur_hex,
            "issuer": holder_str,
            "value": "1000000000000000",
        });
        let issuer_limit = serde_json::json!({
            "currency": cur_hex,
            "issuer": issuer_str,
            "value": "0",
        });
        let (low_limit, high_limit) = if holder_is_low {
            (holder_limit, issuer_limit)
        } else {
            (issuer_limit, holder_limit)
        };

        let obj = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": {
                "currency": cur_hex,
                "issuer": issuer_str,
                "value": balance_str,
            },
            "LowLimit": low_limit,
            "HighLimit": high_limit,
            "Flags": 0,
        });
        let bytes = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TefInternal)?;
        view.insert(tl_key, bytes)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Link only into the holder's owner directory. The AMM pseudo
        // account isn't a real AccountRoot, so it has no owner dir and
        // owner-count bookkeeping is skipped (matches rippled's treatment
        // of pseudo-account-issued LP lines, which are not counted toward
        // a real account's reserve obligation on the issuer side).
        add_to_owner_dir(view, holder, &tl_key)?;

        // Bump the holder's owner count for the new RippleState entry,
        // matching TrustSet's bookkeeping.
        let acct_key = keylet::account(holder);
        if let Some(acct_bytes) = view.read(&acct_key) {
            let mut acct: Value =
                serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
            crate::helpers::adjust_owner_count(&mut acct, 1);
            let new_bytes =
                serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
            view.update(acct_key, new_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
    } else {
        if let Some(bal) = line.get_mut("Balance") {
            bal["value"] = Value::String(balance_str);
        }
        let bytes = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
        view.update(tl_key, bytes)
            .map_err(|_| TransactionResult::TefInternal)?;
    }

    Ok(new_holder_balance as u64)
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
