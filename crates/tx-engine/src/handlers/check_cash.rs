use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::remove_from_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CheckCashTransactor;

fn parse_check_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "CheckID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Hash256::new(arr))
}

impl Transactor for CheckCashTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // CheckID must be present
        parse_check_id(ctx.tx)?;

        // Exactly one of Amount or DeliverMin must be present. Both accept
        // XRP (string) or IOU object.
        let has_amount = ctx.tx.get("Amount").is_some();
        let has_deliver_min = ctx.tx.get("DeliverMin").is_some();
        if has_amount == has_deliver_min {
            return Err(TransactionResult::TemMalformed);
        }
        // Reject Amount/DeliverMin malformed at top level (must be string or object).
        if let Some(a) = ctx.tx.get("Amount") {
            if !a.is_string() && !a.is_object() {
                return Err(TransactionResult::TemBadAmount);
            }
        }
        if let Some(d) = ctx.tx.get("DeliverMin") {
            if !d.is_string() && !d.is_object() {
                return Err(TransactionResult::TemBadAmount);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let check_key = parse_check_id(ctx.tx)?;

        let check_bytes = ctx
            .view
            .read(&check_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let check: serde_json::Value =
            serde_json::from_slice(&check_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // tx Account must be the check's Destination
        let account_str = helpers::get_account(ctx.tx)?;
        let check_dst = check["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if account_str != check_dst {
            return Err(TransactionResult::TecNoPermission);
        }

        // Check must not be expired
        if let Some(expiration) = check.get("Expiration").and_then(|v| v.as_u64()) {
            if (ctx.view.parent_close_time() as u64) >= expiration {
                return Err(TransactionResult::TecExpired);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let check_key = parse_check_id(ctx.tx)?;

        let check_bytes = ctx
            .view
            .read(&check_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let check: serde_json::Value =
            serde_json::from_slice(&check_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let check_src_str = check["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let check_src_id =
            decode_account_id(check_src_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let check_src_key = keylet::account(&check_src_id);

        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let account_key = keylet::account(&account_id);

        // Branch on SendMax shape — XRP (string drops) vs IOU (object).
        let is_iou_check = check
            .get("SendMax")
            .map(|v| v.is_object())
            .unwrap_or(false);

        if !is_iou_check {
            let send_max: u64 = check["SendMax"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or(TransactionResult::TefInternal)?;

            let cash_amount = if let Some(amount) = helpers::get_xrp_amount(ctx.tx) {
                if amount > send_max {
                    return Err(TransactionResult::TecInsufficientPayment);
                }
                amount
            } else {
                let deliver_min = helpers::get_u64_str_field(ctx.tx, "DeliverMin")
                    .ok_or(TransactionResult::TemMalformed)?;
                if deliver_min > send_max {
                    return Err(TransactionResult::TecInsufficientPayment);
                }
                send_max
            };

            let check_src_bytes = ctx
                .view
                .read(&check_src_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut check_src_account: serde_json::Value =
                serde_json::from_slice(&check_src_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let src_balance = helpers::get_balance(&check_src_account);
            if src_balance < cash_amount {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            helpers::set_balance(&mut check_src_account, src_balance - cash_amount);
            helpers::adjust_owner_count(&mut check_src_account, -1);
            let check_src_data =
                serde_json::to_vec(&check_src_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(check_src_key, check_src_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            let account_bytes = ctx
                .view
                .read(&account_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut account: serde_json::Value =
                serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let dst_balance = helpers::get_balance(&account);
            helpers::set_balance(&mut account, dst_balance + cash_amount);
            helpers::increment_sequence(&mut account);
            let account_data =
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(account_key, account_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // IOU CheckCash: SendMax is {currency, issuer, value}. Amount/DeliverMin
            // must match the same currency+issuer. Move IOU from check.Account
            // to tx.Account via their respective trust lines.
            let send_max_obj = check["SendMax"].as_object()
                .ok_or(TransactionResult::TefInternal)?;
            let currency = send_max_obj
                .get("currency")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TefInternal)?;
            let issuer = send_max_obj
                .get("issuer")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TefInternal)?;
            let send_max_value: f64 = send_max_obj
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or(TransactionResult::TefInternal)?;

            // Derive cash amount from tx Amount or DeliverMin.
            let amount_obj = if let Some(a) = ctx.tx.get("Amount").and_then(|v| v.as_object()) {
                a
            } else if let Some(d) = ctx.tx.get("DeliverMin").and_then(|v| v.as_object()) {
                d
            } else {
                return Err(TransactionResult::TemMalformed);
            };
            let amt_currency = amount_obj
                .get("currency")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemBadAmount)?;
            let amt_issuer = amount_obj
                .get("issuer")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemBadAmount)?;
            if amt_currency != currency || amt_issuer != issuer {
                return Err(TransactionResult::TemMalformed);
            }
            let cash_amount: f64 = amount_obj
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or(TransactionResult::TemBadAmount)?;
            if cash_amount > send_max_value {
                return Err(TransactionResult::TecInsufficientPayment);
            }

            let issuer_id = decode_account_id(issuer)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let cur_bytes = helpers::currency_to_bytes(currency);

            // Debit check.Account's trust line.
            let src_trust_key = keylet::trust_line(&check_src_id, &issuer_id, &cur_bytes);
            let src_trust_bytes = ctx
                .view
                .read(&src_trust_key)
                .ok_or(TransactionResult::TecPathDry)?;
            let mut src_trust: serde_json::Value = serde_json::from_slice(&src_trust_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            let src_holder_balance = compute_holder_balance(&src_trust, &issuer_id, &check_src_id);
            if src_holder_balance < cash_amount {
                return Err(TransactionResult::TecPathPartial);
            }
            let new_src_value = adjust_iou_balance(
                &src_trust,
                &format!("-{}", cash_amount),
                &issuer_id,
                &check_src_id,
            )?;
            src_trust["Balance"]["value"] = serde_json::Value::String(new_src_value);
            let src_trust_data =
                serde_json::to_vec(&src_trust).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(src_trust_key, src_trust_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Credit tx.Account's trust line.
            let dst_trust_key = keylet::trust_line(&account_id, &issuer_id, &cur_bytes);
            let dst_trust_bytes = ctx
                .view
                .read(&dst_trust_key)
                .ok_or(TransactionResult::TecPathDry)?;
            let mut dst_trust: serde_json::Value = serde_json::from_slice(&dst_trust_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            let new_dst_value = adjust_iou_balance(
                &dst_trust,
                &cash_amount.to_string(),
                &issuer_id,
                &account_id,
            )?;
            dst_trust["Balance"]["value"] = serde_json::Value::String(new_dst_value);
            let dst_trust_data =
                serde_json::to_vec(&dst_trust).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_trust_key, dst_trust_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Bump check src owner count down + tx sender sequence.
            let src_acct_bytes = ctx
                .view
                .read(&check_src_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut src_acct: serde_json::Value =
                serde_json::from_slice(&src_acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut src_acct, -1);
            let src_acct_data =
                serde_json::to_vec(&src_acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(check_src_key, src_acct_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            let dst_acct_bytes = ctx
                .view
                .read(&account_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut dst_acct: serde_json::Value =
                serde_json::from_slice(&dst_acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut dst_acct);
            let dst_acct_data =
                serde_json::to_vec(&dst_acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(account_key, dst_acct_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Unlink from owner directory then delete the check
        remove_from_owner_dir(ctx.view, &check_src_id, &check_key)?;
        ctx.view
            .erase(&check_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// Re-use Payment's IOU balance helpers via direct re-implementation.
/// (Keeps check_cash decoupled from Payment internals.)
fn compute_holder_balance(
    trust: &serde_json::Value,
    issuer_id: &rxrpl_primitives::AccountId,
    holder_id: &rxrpl_primitives::AccountId,
) -> f64 {
    let raw: f64 = trust
        .get("Balance")
        .and_then(|b| b.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("0")
        .parse()
        .unwrap_or(0.0);
    let issuer_is_low = issuer_id.as_bytes() < holder_id.as_bytes();
    let holder_view = if issuer_is_low { raw } else { -raw };
    holder_view.max(0.0)
}

fn adjust_iou_balance(
    trust: &serde_json::Value,
    delta_str: &str,
    issuer_id: &rxrpl_primitives::AccountId,
    holder_id: &rxrpl_primitives::AccountId,
) -> Result<String, TransactionResult> {
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
    let issuer_is_low = issuer_id.as_bytes() < holder_id.as_bytes();
    let new = if issuer_is_low { current + delta } else { current - delta };
    Ok(if new == new.trunc() {
        format!("{}", new as i64)
    } else {
        format!("{:.15}", new).trim_end_matches('0').trim_end_matches('.').to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_check(src: &str, dst: &str, send_max: u64) -> (Ledger, String) {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(src).unwrap();
        let dst_id = decode_account_id(dst).unwrap();

        for (addr, id, balance) in [(src, &src_id, 100_000_000u64), (dst, &dst_id, 50_000_000)] {
            let key = keylet::account(id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": if addr == src { 1 } else { 0 },
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let check_key = keylet::check(&src_id, 1);
        let check = serde_json::json!({
            "LedgerEntryType": "Check",
            "Account": src,
            "Destination": dst,
            "SendMax": send_max.to_string(),
            "Sequence": 1,
            "Flags": 0,
        });
        ledger
            .put_state(check_key, serde_json::to_vec(&check).unwrap())
            .unwrap();

        let check_id_hex = hex::encode(check_key.as_bytes());
        (ledger, check_id_hex)
    }

    #[test]
    fn preflight_both_amount_and_deliver_min() {
        let tx = serde_json::json!({
            "TransactionType": "CheckCash",
            "Account": DST,
            "CheckID": "0".repeat(64),
            "Amount": "1000000",
            "DeliverMin": "500000",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            CheckCashTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_cashes_check_with_amount() {
        let (ledger, check_id) = setup_with_check(SRC, DST, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "CheckCash",
            "Account": DST,
            "CheckID": check_id,
            "Amount": "3000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CheckCashTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Source debited
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "97000000");

        // Destination credited
        let dst_id = decode_account_id(DST).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "53000000");
    }
}
