use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};
use crate::view::read_view::ReadView;

/// Payment transaction handler.
///
/// Handles XRP-only payments between accounts. Validates source and destination
/// accounts, checks sufficient balance, and transfers drops between accounts.
/// If the destination does not exist, a new AccountRoot is created.
pub struct PaymentTransactor;

impl PaymentTransactor {
    /// Read an AccountRoot from the view and parse it as JSON.
    fn read_account(
        view: &dyn ReadView,
        account_id: &rxrpl_primitives::AccountId,
    ) -> Option<serde_json::Value> {
        let key = keylet::account(account_id);
        let bytes = view.read(&key)?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl Transactor for PaymentTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Destination must be present
        helpers::get_destination(ctx.tx)?;

        let account = helpers::get_account(ctx.tx)?;
        let destination = helpers::get_destination(ctx.tx)?;
        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        // IOU payment: Amount is an object {currency, issuer, value}
        if let Some((_, _, value)) = helpers::get_iou_amount(ctx.tx) {
            let v: f64 = value.parse().map_err(|_| TransactionResult::TemBadAmount)?;
            if v <= 0.0 {
                return Err(TransactionResult::TemBadAmount);
            }
            return Ok(());
        }

        // XRP payment: Amount is a u64 string of drops
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        // Parse source account
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Check source account exists and read its balance
        let src_account =
            Self::read_account(ctx.view, &src_id).ok_or(TransactionResult::TerNoAccount)?;

        // Parse destination account
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Check destination exists
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = ctx.view.read(&dst_key);
        let _dst_exists = dst_bytes.is_some();

        // DepositAuth: if destination has lsfDepositAuth set, the source must
        // either be the destination itself OR be pre-authorized via a
        // DepositPreauth ledger entry. Self-payments are always allowed.
        // RequireDestTag: if destination has lsfRequireDestTag set, the tx
        // must include a DestinationTag.
        if let Some(bytes) = &dst_bytes {
            if let Ok(dst_account) = serde_json::from_slice::<serde_json::Value>(bytes) {
                let dst_flags = dst_account
                    .get("Flags")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                const LSF_DEPOSIT_AUTH: u32 = 0x01000000;
                const LSF_REQUIRE_DEST_TAG: u32 = 0x00020000;
                if dst_flags & LSF_DEPOSIT_AUTH != 0 && account_str != destination_str {
                    let preauth_key = keylet::deposit_preauth(&dst_id, &src_id);
                    if !ctx.view.exists(&preauth_key) {
                        return Err(TransactionResult::TecNoPermission);
                    }
                }
                if dst_flags & LSF_REQUIRE_DEST_TAG != 0
                    && helpers::get_u32_field(ctx.tx, "DestinationTag").is_none()
                {
                    return Err(TransactionResult::TecDstTagNeeded);
                }
            }
        }

        // IOU path: trust line existence is checked in apply; here we only
        // need the source account itself (for fee deduction by the engine).
        if helpers::get_iou_amount(ctx.tx).is_some() {
            return Ok(());
        }

        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);

        // Check source has sufficient balance for amount + fee
        let src_balance = helpers::get_balance(&src_account);
        let total_cost = amount
            .checked_add(fee)
            .ok_or(TransactionResult::TemBadAmount)?;

        if src_balance < total_cost {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        // IOU branch: dispatch to issuer-mint handler.
        if let Some((currency, issuer, value)) = helpers::get_iou_amount(ctx.tx) {
            return apply_iou(ctx, account_str, destination_str, currency, issuer, value);
        }

        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        // Parse account IDs
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let src_key = keylet::account(&src_id);
        let dst_key = keylet::account(&dst_id);

        // Read and update source account
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&src_account);
        let new_src_balance = src_balance
            .checked_sub(amount)
            .ok_or(TransactionResult::TecUnfundedPayment)?;

        helpers::set_balance(&mut src_account, new_src_balance);
        helpers::increment_sequence(&mut src_account);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Read or create destination account
        if let Some(dst_bytes) = ctx.view.read(&dst_key) {
            // Destination exists: add amount to balance
            let mut dst_account: serde_json::Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

            let dst_balance = helpers::get_balance(&dst_account);
            let new_dst_balance = dst_balance
                .checked_add(amount)
                .ok_or(TransactionResult::TefInternal)?;

            helpers::set_balance(&mut dst_account, new_dst_balance);

            let dst_data =
                serde_json::to_vec(&dst_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Destination does not exist: must send at least account_reserve
            // (typically 10 XRP) to fund the new AccountRoot. Otherwise
            // rippled returns tecNO_DST_INSUF_XRP.
            let reserve = ctx.fees.account_reserve(0);
            if amount < reserve {
                return Err(TransactionResult::TecNoDstInsuf);
            }

            // New / resurrected accounts get Sequence = current ledger seq
            // (rippled convention; preserves uniqueness of OfferIDs etc. across
            // delete/recreate cycles within the same ledger history).
            let new_seq = ctx.view.seq().max(1);
            let new_account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": destination_str,
                "Balance": amount.to_string(),
                "Sequence": new_seq,
                "OwnerCount": 0,
                "Flags": 0,
            });

            let dst_data =
                serde_json::to_vec(&new_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
}

/// Apply a Payment whose Amount is an Issued-Currency object.
///
/// Currently scoped to the issuer-mint case (Account == Amount.issuer):
/// the issuer credits the holder's trust line balance for `value` units.
/// Non-issuer IOU sends require trust-line balance arithmetic on the
/// source side and end-to-end pathfinding; left as a follow-up.
fn apply_iou(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    currency: &str,
    issuer: &str,
    value: &str,
) -> Result<TransactionResult, TransactionResult> {
    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dest_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let issuer_id =
        decode_account_id(issuer).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let cur_bytes = helpers::currency_to_bytes(currency);

    if account_str == issuer {
        // Issuer mints IOUs into holder's trust line.
        let trust_key = keylet::trust_line(&issuer_id, &dest_id, &cur_bytes);
        let trust_bytes = ctx
            .view
            .read(&trust_key)
            .ok_or(TransactionResult::TecPathDry)?;
        let mut trust: serde_json::Value =
            serde_json::from_slice(&trust_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let new_value = adjust_iou_balance(&trust, value, &issuer_id, &dest_id)?;
        trust["Balance"]["value"] = serde_json::Value::String(new_value);

        let trust_data = serde_json::to_vec(&trust).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(trust_key, trust_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        let issuer_key = keylet::account(&issuer_id);
        let issuer_bytes = ctx
            .view
            .read(&issuer_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut issuer_acct: serde_json::Value =
            serde_json::from_slice(&issuer_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut issuer_acct);
        let issuer_data =
            serde_json::to_vec(&issuer_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(issuer_key, issuer_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        return Ok(TransactionResult::TesSuccess);
    }

    // Non-issuer IOU send: debit source's RippleState. If the destination is
    // the issuer itself, this is a burn (only debit). Otherwise also credit
    // destination's RippleState with the issuer.
    let send_value: f64 = value.parse().map_err(|_| TransactionResult::TemBadAmount)?;
    if send_value <= 0.0 {
        return Err(TransactionResult::TemBadAmount);
    }

    // Source debits ITS trust line balance toward issuer.
    let src_trust_key = keylet::trust_line(&src_id, &issuer_id, &cur_bytes);
    let src_trust_bytes = ctx
        .view
        .read(&src_trust_key)
        .ok_or(TransactionResult::TecPathDry)?;
    let mut src_trust: serde_json::Value =
        serde_json::from_slice(&src_trust_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let new_src_value =
        adjust_iou_balance(&src_trust, &format!("-{}", value), &issuer_id, &src_id)?;
    // Source must have sufficient balance (cannot go below 0 from holder's perspective).
    let src_holder_balance = compute_holder_balance(&src_trust, &issuer_id, &src_id);
    if src_holder_balance < send_value {
        return Err(TransactionResult::TecPathPartial);
    }
    src_trust["Balance"]["value"] = serde_json::Value::String(new_src_value);
    let src_trust_data =
        serde_json::to_vec(&src_trust).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_trust_key, src_trust_data)
        .map_err(|_| TransactionResult::TefInternal)?;

    // If destination is the issuer, we just burned IOU -- no trust line on
    // the issuer's side to credit. Otherwise credit destination's RippleState.
    if dest_id != issuer_id {
        let dst_trust_key = keylet::trust_line(&dest_id, &issuer_id, &cur_bytes);
        let dst_trust_bytes = ctx
            .view
            .read(&dst_trust_key)
            .ok_or(TransactionResult::TecPathDry)?;
        let mut dst_trust: serde_json::Value = serde_json::from_slice(&dst_trust_bytes)
            .map_err(|_| TransactionResult::TefInternal)?;

        let new_dst_value = adjust_iou_balance(&dst_trust, value, &issuer_id, &dest_id)?;
        dst_trust["Balance"]["value"] = serde_json::Value::String(new_dst_value);
        let dst_trust_data =
            serde_json::to_vec(&dst_trust).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(dst_trust_key, dst_trust_data)
            .map_err(|_| TransactionResult::TefInternal)?;
    }

    // Bump source Sequence.
    let src_key = keylet::account(&src_id);
    let src_bytes = ctx
        .view
        .read(&src_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut src_acct: serde_json::Value =
        serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::increment_sequence(&mut src_acct);
    let src_acct_data =
        serde_json::to_vec(&src_acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_key, src_acct_data)
        .map_err(|_| TransactionResult::TefInternal)?;

    Ok(TransactionResult::TesSuccess)
}

/// Compute holder's IOU balance against the issuer (always non-negative
/// from holder's perspective). Returns 0 if holder owes issuer.
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

/// Compute the new RippleState Balance.value after an issuer mint.
///
/// RippleState Balance is stored from the low-account perspective.
/// `+value` means the high account owes the low account that much;
/// `-value` means the low account owes the high account.
/// When the issuer mints to the holder:
/// - issuer = low → holder owes more → balance += delta
/// - issuer = high → holder owes more (to high) → balance -= delta
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
    let new = if issuer_is_low {
        current + delta
    } else {
        current - delta
    };
    Ok(format_iou_value(new))
}

/// Render an IOU value back as a string in a stable form.
fn format_iou_value(v: f64) -> String {
    if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC_ADDRESS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST_ADDRESS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let data = serde_json::to_vec(&account).unwrap();
        ledger.put_state(key, data).unwrap();
        ledger
    }

    fn add_account(ledger: &mut Ledger, address: &str, balance: u64) {
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn make_payment_tx(
        account: &str,
        destination: &str,
        amount: &str,
        fee: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "Payment",
            "Account": account,
            "Destination": destination,
            "Amount": amount,
            "Fee": fee,
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": SRC_ADDRESS,
            "Amount": "1000000",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemDstIsObligatory));
    }

    #[test]
    fn preflight_missing_amount() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": SRC_ADDRESS,
            "Destination": DST_ADDRESS,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadAmount));
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "0", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadAmount));
    }

    #[test]
    fn preflight_self_payment() {
        let tx = make_payment_tx(SRC_ADDRESS, SRC_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadSend));
    }

    #[test]
    fn preflight_valid() {
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(PaymentTransactor.preflight(&ctx).is_ok());
    }

    // -- preclaim tests --

    #[test]
    fn preclaim_source_not_found() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        let result = PaymentTransactor.preclaim(&ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }

    #[test]
    fn preclaim_insufficient_balance() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        let result = PaymentTransactor.preclaim(&ctx);
        assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
    }

    #[test]
    fn preclaim_valid_with_existing_destination() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(PaymentTransactor.preclaim(&ctx).is_ok());
    }

    #[test]
    fn preclaim_valid_create_account() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(PaymentTransactor.preclaim(&ctx).is_ok());
    }

    // -- apply tests --

    #[test]
    fn apply_transfer_to_existing_account() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify source balance decreased and sequence incremented
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "9000000");
        assert_eq!(src["Sequence"].as_u64().unwrap(), 2);

        // Verify destination balance increased
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "6000000");
    }

    #[test]
    fn apply_creates_new_destination_account() {
        // Funding a new account requires sending at least account_reserve.
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 50_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "10000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify destination was created with correct fields
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "10000000");
        assert_eq!(dst["LedgerEntryType"].as_str().unwrap(), "AccountRoot");
        assert_eq!(dst["Sequence"].as_u64().unwrap(), 1);
        assert_eq!(dst["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_below_reserve_fails_to_create_destination() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 50_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // 1 drop is way below the 10 XRP reserve.
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TecNoDstInsuf));
    }

    #[test]
    fn apply_insufficient_balance() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
    }

    #[test]
    fn apply_source_not_found() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }
}
