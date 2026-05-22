use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amount_helpers::{compute_holder_balance, compute_new_iou_balance};
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
            // Cross-currency: SendMax in a different currency than Amount means
            // the payment must flow through the order book.
            if let Some((sm_cur, sm_iss, sm_val)) = get_send_max_iou(ctx.tx) {
                if sm_cur != currency || sm_iss != issuer {
                    return apply_cross_currency(
                        ctx,
                        account_str,
                        destination_str,
                        (currency, issuer, value),
                        (sm_cur, sm_iss, sm_val),
                    );
                }
            }
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
            // PreviousTxnID + PreviousTxnLgrSeq are SOE_REQUIRED on rippled's
            // AccountRoot SOTemplate. Omitting them produced parse-time
            // throws ("Field 'PreviousTxnID' is required but missing.") on
            // any rippled peer that received the SLE — most visibly when a
            // late-joining rippled tried `account_info` against the rxrpl
            // network and got rpcINTERNAL. We don't yet plumb the apply
            // tx-hash into ApplyContext, so PreviousTxnID is set to zero;
            // PreviousTxnLgrSeq is the ledger this tx is being applied in.
            // Follow-up: thread real tx-hash through `ApplyContext` for full
            // ancestry traceability.
            let new_account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": destination_str,
                "Balance": amount.to_string(),
                "Sequence": new_seq,
                "OwnerCount": 0,
                "Flags": 0,
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": new_seq,
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

    // GlobalFreeze: when the issuer has lsfGlobalFreeze set, only the issuer
    // can move its IOU. Non-issuer transfers must fail.
    // Per rippled's RippleCalc/PathTransfer logic, a strand encountering a
    // frozen issuer fails with tecPATH_DRY.
    const LSF_GLOBAL_FREEZE: u32 = 0x00400000;
    if account_str != issuer {
        let issuer_key = keylet::account(&issuer_id);
        if let Some(bytes) = ctx.view.read(&issuer_key) {
            if let Ok(issuer_acct) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                let issuer_flags = issuer_acct
                    .get("Flags")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                if issuer_flags & LSF_GLOBAL_FREEZE != 0 {
                    return Err(TransactionResult::TecPathDry);
                }
            }
        }
    }

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

    // TransferRate: when the issuer charges a transfer fee and is not party
    // to the transfer, the source is debited `value * rate` while the
    // destination still receives `value`. The grossed-up debit must fit
    // within SendMax if one was supplied.
    let rate = issuer_transfer_rate(ctx, &issuer_id);
    let src_debit_value = if dest_id == issuer_id {
        send_value
    } else {
        send_value * rate
    };
    if let Some((sm_cur, sm_iss, sm_val)) = get_send_max_iou(ctx.tx) {
        if sm_cur == currency && sm_iss == issuer {
            let sm: f64 = sm_val.parse().unwrap_or(0.0);
            if src_debit_value > sm + 1e-9 {
                return Err(TransactionResult::TecPathPartial);
            }
        }
    }

    // Source debits ITS trust line balance toward issuer.
    let src_trust_key = keylet::trust_line(&src_id, &issuer_id, &cur_bytes);
    let src_trust_bytes = ctx
        .view
        .read(&src_trust_key)
        .ok_or(TransactionResult::TecPathDry)?;
    let mut src_trust: serde_json::Value =
        serde_json::from_slice(&src_trust_bytes).map_err(|_| TransactionResult::TefInternal)?;

    let new_src_value = adjust_iou_balance(
        &src_trust,
        &format!("-{}", src_debit_value),
        &issuer_id,
        &src_id,
    )?;
    // Source must have sufficient balance (cannot go below 0 from holder's perspective).
    let src_holder_balance = compute_holder_balance(&src_trust, &issuer_id, &src_id);
    if src_holder_balance < src_debit_value - 1e-9 {
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
        let mut dst_trust: serde_json::Value =
            serde_json::from_slice(&dst_trust_bytes).map_err(|_| TransactionResult::TefInternal)?;

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
    let new = compute_new_iou_balance(trust, delta_str, issuer_id, holder_id)?;
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

/// Extract a SendMax IOU object as (currency, issuer, value).
fn get_send_max_iou(tx: &serde_json::Value) -> Option<(&str, &str, &str)> {
    let sm = tx.get("SendMax")?;
    if !sm.is_object() {
        return None;
    }
    Some((
        sm.get("currency")?.as_str()?,
        sm.get("issuer")?.as_str()?,
        sm.get("value")?.as_str()?,
    ))
}

/// Extract a DeliverMin IOU object as (currency, issuer, value). Returned
/// only when `DeliverMin` is set as an IOU object; XRP-amount variants are
/// not yet supported here (they only matter for XRP-only Payments).
fn get_deliver_min_iou(tx: &serde_json::Value) -> Option<(&str, &str, &str)> {
    let dm = tx.get("DeliverMin")?;
    if !dm.is_object() {
        return None;
    }
    Some((
        dm.get("currency")?.as_str()?,
        dm.get("issuer")?.as_str()?,
        dm.get("value")?.as_str()?,
    ))
}

/// Read the issuer's TransferRate as a multiplier (1.0 = no fee).
fn issuer_transfer_rate(ctx: &ApplyContext<'_>, issuer_id: &rxrpl_primitives::AccountId) -> f64 {
    let key = keylet::account(issuer_id);
    ctx.view
        .read(&key)
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|a| a.get("TransferRate").and_then(|v| v.as_u64()))
        .map(|r| {
            if r > 1_000_000_000 {
                r as f64 / 1_000_000_000.0
            } else {
                1.0
            }
        })
        .unwrap_or(1.0)
}

/// Adjust a holder's RippleState balance toward an issuer by `delta`
/// (positive = credit holder, negative = debit holder).
fn apply_trust_delta(
    ctx: &mut ApplyContext<'_>,
    holder_id: &rxrpl_primitives::AccountId,
    issuer_id: &rxrpl_primitives::AccountId,
    cur_bytes: &[u8; 20],
    delta: f64,
) -> Result<(), TransactionResult> {
    let key = keylet::trust_line(holder_id, issuer_id, cur_bytes);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TecPathDry)?;
    let mut trust: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    if delta < 0.0 {
        let holder_balance = compute_holder_balance(&trust, issuer_id, holder_id);
        if holder_balance < -delta - 1e-9 {
            return Err(TransactionResult::TecPathPartial);
        }
    }
    let new_value = adjust_iou_balance(&trust, &format!("{delta}"), issuer_id, holder_id)?;
    trust["Balance"]["value"] = serde_json::Value::String(new_value);
    let data = serde_json::to_vec(&trust).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, data)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// An order-book offer with its parsed IOU amounts.
struct CrossOffer {
    key: rxrpl_primitives::Hash256,
    owner: rxrpl_primitives::AccountId,
    taker_pays: f64,
    taker_gets: f64,
}

/// Apply a cross-currency Payment: the source pays `send_max` (currency A)
/// and the destination receives `amount` (currency B), bridged through the
/// order book. Scoped to a single IOU->IOU hop via offers that sell B for A
/// (book keyed by pays = A, gets = B).
///
/// When `Paths` is present with a single intermediate currency step, the
/// payment routes through two book crossings (A -> intermediate -> B). The
/// fallback path (no Paths or unsupported shape) drives the legacy direct
/// A/B book lookup.
fn apply_cross_currency(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: (&str, &str, &str),
    send_max: (&str, &str, &str),
) -> Result<TransactionResult, TransactionResult> {
    // Multi-hop dispatch: when `Paths` is present, dry-run a back-solve for
    // each viable Path (pure currency-change chain), then commit mutations
    // for the alternative with the lowest source-currency spend. That mirrors
    // rippled's RippleCalc quality-based ranking: the same target Amount is
    // delivered with less SendMax spent, so the source pays the minimum.
    // Paths the back-solve cannot satisfy (insufficient liquidity, books
    // missing, mismatched issuer/currency, etc.) are skipped. If no Path
    // succeeds, fall through to the legacy direct-book lookup below.
    // tfPartialPayment allows delivering less than `Amount` when the
    // requested target is unreachable within `SendMax`. The minimum
    // acceptable delivery is given by `DeliverMin` (defaults to `Amount`,
    // i.e. partial mode disabled for paths that can reach the full target).
    const TF_PARTIAL_PAYMENT: u32 = rxrpl_protocol::flags::payment::TF_PARTIAL_PAYMENT;
    let partial_allowed = helpers::get_flags(ctx.tx) & TF_PARTIAL_PAYMENT != 0;
    let deliver_min: Option<f64> = get_deliver_min_iou(ctx.tx).and_then(|(cur, iss, val)| {
        if cur == amount.0 && iss == amount.1 {
            val.parse::<f64>().ok()
        } else {
            None
        }
    });
    // DeliverMin must not exceed Amount when both are present.
    if let Some(d_min) = deliver_min {
        let target_val: f64 = amount
            .2
            .parse()
            .map_err(|_| TransactionResult::TemBadAmount)?;
        if d_min > target_val + 1e-9 {
            return Err(TransactionResult::TemBadAmount);
        }
    }

    if let Some(paths) = ctx.tx.get("Paths").and_then(|v| v.as_array()) {
        let (src_cur_for_inherit, src_iss_for_inherit, _) = send_max;
        let mut best: Option<NHopPlan> = None;
        for path in paths {
            let Some(intermediates) = path.as_array().and_then(|p| {
                simple_path_intermediates(p, src_cur_for_inherit, src_iss_for_inherit)
            }) else {
                continue;
            };
            if intermediates.is_empty() {
                continue;
            }
            let Ok(plan) = back_solve_n_hop(
                ctx,
                account_str,
                destination_str,
                amount,
                send_max,
                &intermediates,
                partial_allowed,
                deliver_min,
            ) else {
                continue;
            };
            best = Some(match best.take() {
                Some(prev) if prev.src_spent <= plan.src_spent => prev,
                _ => plan,
            });
        }
        if let Some(plan) = best {
            return commit_n_hop_plan(ctx, plan);
        }
    }
    let (dst_cur, dst_iss, dst_val) = amount;
    let (src_cur, src_iss, src_max) = send_max;

    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dest_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dst_issuer_id =
        decode_account_id(dst_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let src_issuer_id =
        decode_account_id(src_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?;

    let dst_cur_bytes = helpers::currency_to_bytes(dst_cur);
    let src_cur_bytes = helpers::currency_to_bytes(src_cur);

    let target: f64 = dst_val
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    let send_max_val: f64 = src_max
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    if target <= 0.0 {
        return Err(TransactionResult::TemBadAmount);
    }

    // Book of offers selling dst currency for src currency:
    // takers pay src, takers get dst.
    let book_root = keylet::book_dir(
        &src_cur_bytes,
        &src_issuer_id,
        &dst_cur_bytes,
        &dst_issuer_id,
    );
    let offers = collect_book_offers(ctx, &book_root);

    let mut remaining = target;
    let mut src_spent = 0.0;
    let mut consumed: Vec<(
        rxrpl_primitives::Hash256,
        rxrpl_primitives::AccountId,
        f64,
        f64,
    )> = Vec::new();

    for offer in &offers {
        if remaining <= 1e-9 {
            break;
        }
        if offer.taker_gets <= 0.0 || offer.taker_pays <= 0.0 {
            continue;
        }
        let take_dst = remaining.min(offer.taker_gets);
        let take_src = take_dst * offer.taker_pays / offer.taker_gets;
        if src_spent + take_src > send_max_val + 1e-9 {
            return Err(TransactionResult::TecPathPartial);
        }
        consumed.push((offer.key, offer.owner, take_src, take_dst));
        remaining -= take_dst;
        src_spent += take_src;
    }

    if remaining > 1e-9 {
        return Err(TransactionResult::TecPathPartial);
    }

    // Debit source's src-currency trust line by total spent.
    apply_trust_delta(ctx, &src_id, &src_issuer_id, &src_cur_bytes, -src_spent)?;

    for (offer_key, owner_id, take_src, take_dst) in &consumed {
        // Offer owner receives src currency and gives up dst currency.
        if *owner_id != src_issuer_id {
            apply_trust_delta(ctx, owner_id, &src_issuer_id, &src_cur_bytes, *take_src)?;
        }
        if *owner_id != dst_issuer_id {
            apply_trust_delta(ctx, owner_id, &dst_issuer_id, &dst_cur_bytes, -*take_dst)?;
        }
        update_consumed_offer(ctx, offer_key, &book_root, *take_src, *take_dst)?;
    }

    // Credit destination's dst-currency trust line.
    apply_trust_delta(ctx, &dest_id, &dst_issuer_id, &dst_cur_bytes, target)?;

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

/// Inspect a single Path (array of step objects) and resolve each step's
/// effective `(currency, issuer)` by walking the inheritance chain from
/// the source side. Returns the fully resolved chain of intermediates in
/// walk order, or `None` if any step is account-rippling (`type & 0x01`,
/// reserved for Phase 3b) or carries no `0x10`/`0x20` bits at all.
///
/// rippled's PathStep bitmap:
///   * `0x10` (PATH_STEP_CURRENCY): currency comes from the step
///   * `0x20` (PATH_STEP_ISSUER): issuer comes from the step
///   * `0x30` (combined): both come from the step
///   * `0x01` (PATH_STEP_ACCOUNT): account-rippling, NOT supported here
///
/// For a `0x10`-only step the issuer is inherited from the previous hop
/// (or the source side for the first step). For a `0x20`-only step the
/// currency is inherited the same way. This is how cross-issuer hops
/// (USD@A → USD@B via a `0x20` step) and currency-only chains are
/// expressed in rippled's wire format.
fn simple_path_intermediates(
    path: &Vec<serde_json::Value>,
    src_cur: &str,
    src_iss: &str,
) -> Option<Vec<(String, String)>> {
    const PATH_STEP_ACCOUNT: u64 = 0x01;
    const PATH_STEP_CURRENCY: u64 = 0x10;
    const PATH_STEP_ISSUER: u64 = 0x20;

    let mut out = Vec::with_capacity(path.len());
    let mut current_cur = src_cur.to_string();
    let mut current_iss = src_iss.to_string();
    for step in path {
        let step_type = step.get("type").and_then(|v| v.as_u64()).unwrap_or(0);
        if step_type & PATH_STEP_ACCOUNT != 0 {
            // Account-rippling steps are Phase 3b territory; fall through to
            // alternative Paths or the legacy direct-book code.
            return None;
        }
        let has_currency = step_type & PATH_STEP_CURRENCY != 0;
        let has_issuer = step_type & PATH_STEP_ISSUER != 0;
        if !has_currency && !has_issuer {
            return None;
        }
        if has_currency {
            current_cur = step.get("currency").and_then(|v| v.as_str())?.to_string();
        }
        if has_issuer {
            current_iss = step.get("issuer").and_then(|v| v.as_str())?.to_string();
        }
        out.push((current_cur.clone(), current_iss.clone()));
    }
    Some(out)
}

/// Resolved N-hop payment plan ready for application. Produced by
/// `back_solve_n_hop` (read-only) and consumed by `commit_n_hop_plan`
/// (mutating). Splitting the two phases lets the multi-path dispatch
/// dry-run every Path alternative and pick the one with the lowest
/// `src_spent` before touching the ledger.
struct NHopPlan {
    src_id: rxrpl_primitives::AccountId,
    dest_id: rxrpl_primitives::AccountId,
    target: f64,
    src_spent: f64,
    /// Currency + issuer per hop boundary, `chain[0]` = source side,
    /// `chain.last()` = destination side. Length = `hop_count + 1`.
    chain: Vec<([u8; 20], rxrpl_primitives::AccountId)>,
    /// Per-hop book directory keylet.
    hop_books: Vec<rxrpl_primitives::Hash256>,
    /// Per-hop list of `(offer_key, owner, take_in, take_out)` to apply.
    hop_consumed: Vec<
        Vec<(
            rxrpl_primitives::Hash256,
            rxrpl_primitives::AccountId,
            f64,
            f64,
        )>,
    >,
}

/// Dry-run an N-hop cross-currency Payment: build the source→intermediates
/// →destination chain, back-solve liquidity from destination toward source,
/// and return the resolved `NHopPlan`. No mutations are applied — every
/// read is via `collect_book_offers` and the surrounding sandbox view.
///
/// When `partial_allowed` is `true` and the back-solve's required source
/// spend exceeds `SendMax`, the plan is scaled down linearly so the actual
/// delivered amount is the maximum reachable within `SendMax`. The scaled
/// delivery must still be at least `deliver_min` (when supplied) or the
/// call fails `TecPathPartial`. Without `partial_allowed`, any shortfall
/// (insufficient liquidity at any hop OR `src_spent > SendMax`) returns
/// `TecPathPartial`.
fn back_solve_n_hop(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: (&str, &str, &str),
    send_max: (&str, &str, &str),
    intermediates: &[(String, String)],
    partial_allowed: bool,
    deliver_min: Option<f64>,
) -> Result<NHopPlan, TransactionResult> {
    let (dst_cur, dst_iss, dst_val) = amount;
    let (src_cur, src_iss, src_max) = send_max;

    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dest_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

    let target: f64 = dst_val
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    let send_max_val: f64 = src_max
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    if target <= 0.0 {
        return Err(TransactionResult::TemBadAmount);
    }

    // Build the (currency, issuer) chain: src -> int1 -> ... -> intN -> dst.
    let mut chain: Vec<([u8; 20], rxrpl_primitives::AccountId)> = Vec::new();
    chain.push((
        helpers::currency_to_bytes(src_cur),
        decode_account_id(src_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
    ));
    for (cur, iss) in intermediates {
        chain.push((
            helpers::currency_to_bytes(cur),
            decode_account_id(iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
        ));
    }
    chain.push((
        helpers::currency_to_bytes(dst_cur),
        decode_account_id(dst_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
    ));

    let hop_count = chain.len() - 1;
    let mut hop_books: Vec<rxrpl_primitives::Hash256> = Vec::with_capacity(hop_count);
    let mut hop_consumed: Vec<
        Vec<(
            rxrpl_primitives::Hash256,
            rxrpl_primitives::AccountId,
            f64,
            f64,
        )>,
    > = vec![Vec::new(); hop_count];

    let mut required_output = target;
    for hop in (0..hop_count).rev() {
        let (in_cur, in_iss) = &chain[hop];
        let (out_cur, out_iss) = &chain[hop + 1];
        let book = keylet::book_dir(in_cur, in_iss, out_cur, out_iss);
        let offers = collect_book_offers(ctx, &book);
        hop_books.push(book);

        let mut remaining = required_output;
        let mut consumed: Vec<(_, _, _, _)> = Vec::new();
        let mut input_required = 0.0;
        for offer in &offers {
            if remaining <= 1e-9 {
                break;
            }
            if offer.taker_gets <= 0.0 || offer.taker_pays <= 0.0 {
                continue;
            }
            let take_out = remaining.min(offer.taker_gets);
            let take_in = take_out * offer.taker_pays / offer.taker_gets;
            consumed.push((offer.key, offer.owner, take_in, take_out));
            remaining -= take_out;
            input_required += take_in;
        }
        if remaining > 1e-9 {
            return Err(TransactionResult::TecPathPartial);
        }
        hop_consumed[hop] = consumed;
        required_output = input_required;
    }
    hop_books.reverse();

    let src_spent_full = required_output;
    if src_spent_full <= send_max_val + 1e-9 {
        return Ok(NHopPlan {
            src_id,
            dest_id,
            target,
            src_spent: src_spent_full,
            chain,
            hop_books,
            hop_consumed,
        });
    }

    // Source spend exceeds SendMax. Without partial mode, fail. With it,
    // scale all consumption proportionally so the actual delivered amount
    // is the maximum reachable within SendMax. The flow is linear in each
    // hop (offer slices are taken proportionally to the desired output),
    // so a uniform scale factor `scale = send_max / src_spent_full`
    // preserves the per-offer ratios.
    if !partial_allowed {
        return Err(TransactionResult::TecPathPartial);
    }
    let scale = send_max_val / src_spent_full;
    let scaled_target = target * scale;
    if let Some(d_min) = deliver_min {
        if scaled_target + 1e-9 < d_min {
            return Err(TransactionResult::TecPathPartial);
        }
    } else if scaled_target + 1e-9 < target {
        // No DeliverMin specified: partial mode requires Amount delivered
        // in full. (rippled's behavior — tfPartialPayment without
        // DeliverMin still requires Amount or fails.)
        return Err(TransactionResult::TecPathPartial);
    }
    for hop in hop_consumed.iter_mut() {
        for entry in hop.iter_mut() {
            entry.2 *= scale;
            entry.3 *= scale;
        }
    }

    Ok(NHopPlan {
        src_id,
        dest_id,
        target: scaled_target,
        src_spent: send_max_val,
        chain,
        hop_books,
        hop_consumed,
    })
}

/// Apply the mutations for a previously back-solved `NHopPlan`. Walks hops
/// in forward order, applies trust-line deltas to the source, each
/// market-maker, and the destination, updates each consumed offer, and
/// bumps the source `Sequence`. All-or-nothing within the sandbox.
fn commit_n_hop_plan(
    ctx: &mut ApplyContext<'_>,
    plan: NHopPlan,
) -> Result<TransactionResult, TransactionResult> {
    let NHopPlan {
        src_id,
        dest_id,
        target,
        src_spent,
        chain,
        hop_books,
        hop_consumed,
    } = plan;
    let hop_count = chain.len() - 1;

    let (first_cur, first_iss) = &chain[0];
    apply_trust_delta(ctx, &src_id, first_iss, first_cur, -src_spent)?;

    for hop in 0..hop_count {
        let (in_cur, in_iss) = &chain[hop];
        let (out_cur, out_iss) = &chain[hop + 1];
        let book = &hop_books[hop];
        for (offer_key, owner_id, take_in, take_out) in &hop_consumed[hop] {
            if *owner_id != *in_iss {
                apply_trust_delta(ctx, owner_id, in_iss, in_cur, *take_in)?;
            }
            if *owner_id != *out_iss {
                apply_trust_delta(ctx, owner_id, out_iss, out_cur, -*take_out)?;
            }
            update_consumed_offer(ctx, offer_key, book, *take_in, *take_out)?;
        }
    }

    let (last_cur, last_iss) = &chain[chain.len() - 1];
    apply_trust_delta(ctx, &dest_id, last_iss, last_cur, target)?;

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

/// Walk a book directory and collect its offers with parsed IOU amounts.
fn collect_book_offers(
    ctx: &mut ApplyContext<'_>,
    book_root: &rxrpl_primitives::Hash256,
) -> Vec<CrossOffer> {
    let mut out = Vec::new();
    let mut page = 0u64;
    loop {
        let page_key = keylet::dir_node(book_root, page);
        let page_bytes = match ctx.view.read(&page_key) {
            Some(b) => b,
            None => break,
        };
        let page_json: serde_json::Value = match serde_json::from_slice(&page_bytes) {
            Ok(v) => v,
            Err(_) => break,
        };
        if let Some(indexes) = page_json.get("Indexes").and_then(|v| v.as_array()) {
            for idx in indexes {
                let Some(s) = idx.as_str() else { continue };
                let Ok(h) = s.parse::<rxrpl_primitives::Hash256>() else {
                    continue;
                };
                let Some(eb) = ctx.view.read(&h) else {
                    continue;
                };
                let Ok(entry) = serde_json::from_slice::<serde_json::Value>(&eb) else {
                    continue;
                };
                if entry.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                    continue;
                }
                let owner = entry
                    .get("Account")
                    .and_then(|v| v.as_str())
                    .and_then(|s| decode_account_id(s).ok());
                let Some(owner) = owner else { continue };
                let taker_pays = iou_value(entry.get("TakerPays"));
                let taker_gets = iou_value(entry.get("TakerGets"));
                out.push(CrossOffer {
                    key: h,
                    owner,
                    taker_pays,
                    taker_gets,
                });
            }
        }
        match page_json.get("IndexNext").and_then(|v| v.as_u64()) {
            Some(next) if next != 0 => page = next,
            _ => break,
        }
    }
    out
}

/// Parse the numeric value of an IOU (or drops) amount field.
fn iou_value(v: Option<&serde_json::Value>) -> f64 {
    match v {
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(0.0),
        Some(obj) => obj
            .get("value")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0),
        None => 0.0,
    }
}

/// Reduce a consumed offer's remaining amounts; erase it if fully filled.
fn update_consumed_offer(
    ctx: &mut ApplyContext<'_>,
    offer_key: &rxrpl_primitives::Hash256,
    book_root: &rxrpl_primitives::Hash256,
    take_pays: f64,
    take_gets: f64,
) -> Result<(), TransactionResult> {
    let bytes = ctx
        .view
        .read(offer_key)
        .ok_or(TransactionResult::TefInternal)?;
    let mut offer: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let pays = iou_value(offer.get("TakerPays"));
    let gets = iou_value(offer.get("TakerGets"));
    let new_pays = (pays - take_pays).max(0.0);
    let new_gets = (gets - take_gets).max(0.0);

    if new_gets <= 1e-9 || new_pays <= 1e-9 {
        let owner = offer
            .get("Account")
            .and_then(|v| v.as_str())
            .and_then(|s| decode_account_id(s).ok());
        if let Some(owner_id) = owner {
            crate::owner_dir::remove_from_owner_dir(ctx.view, &owner_id, offer_key)?;
            let owner_key = keylet::account(&owner_id);
            if let Some(b) = ctx.view.read(&owner_key) {
                if let Ok(mut acct) = serde_json::from_slice::<serde_json::Value>(&b) {
                    helpers::adjust_owner_count(&mut acct, -1);
                    if let Ok(nb) = serde_json::to_vec(&acct) {
                        let _ = ctx.view.update(owner_key, nb);
                    }
                }
            }
        }
        remove_offer_from_book(ctx.view, book_root, offer_key)?;
        let _ = ctx.view.erase(offer_key);
        return Ok(());
    }

    if let Some(tp) = offer.get_mut("TakerPays") {
        if tp.is_object() {
            tp["value"] = serde_json::Value::String(format_iou_value(new_pays));
        }
    }
    if let Some(tg) = offer.get_mut("TakerGets") {
        if tg.is_object() {
            tg["value"] = serde_json::Value::String(format_iou_value(new_gets));
        }
    }
    let data = serde_json::to_vec(&offer).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(*offer_key, data)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// Remove an offer hash from a book directory page.
fn remove_offer_from_book(
    view: &mut dyn crate::view::apply_view::ApplyView,
    book_root: &rxrpl_primitives::Hash256,
    offer_key: &rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    let entry_hex = offer_key.to_string();
    let mut page = 0u64;
    loop {
        let page_key = keylet::dir_node(book_root, page);
        let bytes = match view.read(&page_key) {
            Some(b) => b,
            None => return Ok(()),
        };
        let mut dir: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let next_page = dir.get("IndexNext").and_then(|v| v.as_u64()).unwrap_or(0);
        let removed = if let Some(indexes) = dir.get_mut("Indexes").and_then(|v| v.as_array_mut()) {
            let original = indexes.len();
            indexes.retain(|v| v.as_str() != Some(entry_hex.as_str()));
            indexes.len() != original
        } else {
            false
        };
        if removed {
            let empty = dir
                .get("Indexes")
                .and_then(|v| v.as_array())
                .map(|a| a.is_empty())
                .unwrap_or(true);
            if empty && page == 0 {
                let _ = view.erase(&page_key);
            } else {
                let new_bytes =
                    serde_json::to_vec(&dir).map_err(|_| TransactionResult::TefInternal)?;
                let _ = view.update(page_key, new_bytes);
            }
            return Ok(());
        }
        if next_page == 0 {
            return Ok(());
        }
        page = next_page;
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

    // -- transfer rate / cross-currency tests --

    const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const ALICE: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const BOB: &str = "rGWrZyQqhTp9Xu7G5Pkayo7bXjH4k4QYpf";
    const MM: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";

    fn put_account(ledger: &mut Ledger, addr: &str, balance: &str, transfer_rate: Option<u64>) {
        let id = decode_account_id(addr).unwrap();
        let key = keylet::account(&id);
        let mut acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": addr,
            "Balance": balance,
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        if let Some(rate) = transfer_rate {
            acct["TransferRate"] = serde_json::Value::from(rate);
        }
        ledger
            .put_state(key, serde_json::to_vec(&acct).unwrap())
            .unwrap();
    }

    /// Insert a RippleState giving `holder` a positive `value` of `currency`
    /// from `issuer`.
    fn put_trust_line(ledger: &mut Ledger, holder: &str, issuer: &str, currency: &str, value: f64) {
        let holder_id = decode_account_id(holder).unwrap();
        let issuer_id = decode_account_id(issuer).unwrap();
        let cur_bytes = helpers::currency_to_bytes(currency);
        let key = keylet::trust_line(&holder_id, &issuer_id, &cur_bytes);

        let holder_is_low = holder_id.as_bytes() < issuer_id.as_bytes();
        let stored = if holder_is_low { value } else { -value };
        let (low_addr, high_addr) = if holder_is_low {
            (holder, issuer)
        } else {
            (issuer, holder)
        };
        let tl = serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": { "currency": currency, "issuer": issuer, "value": format!("{stored}") },
            "LowLimit": { "currency": currency, "issuer": low_addr, "value": "0" },
            "HighLimit": { "currency": currency, "issuer": high_addr, "value": "1000" },
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&tl).unwrap())
            .unwrap();
    }

    fn iou(currency: &str, issuer: &str, value: &str) -> serde_json::Value {
        serde_json::json!({ "currency": currency, "issuer": issuer, "value": value })
    }

    fn holder_balance(view: &dyn ReadView, holder: &str, issuer: &str, currency: &str) -> f64 {
        let holder_id = decode_account_id(holder).unwrap();
        let issuer_id = decode_account_id(issuer).unwrap();
        let cur_bytes = helpers::currency_to_bytes(currency);
        let key = keylet::trust_line(&holder_id, &issuer_id, &cur_bytes);
        let bytes = view.read(&key).unwrap();
        let tl: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        compute_holder_balance(&tl, &issuer_id, &holder_id)
    }

    #[test]
    fn apply_iou_transfer_rate_deducts_fee_from_source() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", Some(1_200_000_000));
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "USD", 0.0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("USD", ISSUER, "50"),
            "SendMax": iou("USD", ISSUER, "60"),
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 40.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "USD") - 50.0).abs() < 1e-6);
    }

    #[test]
    fn apply_iou_transfer_rate_send_max_too_low_fails() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", Some(1_200_000_000));
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "USD", 0.0);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("USD", ISSUER, "50"),
            "SendMax": iou("USD", ISSUER, "55"),
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TecPathPartial));
    }

    #[test]
    fn apply_cross_currency_consumes_offer() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);

        // MM offer: TakerPays USD 50, TakerGets EUR 50.
        let mm_id = decode_account_id(MM).unwrap();
        let offer_key = keylet::offer(&mm_id, 1);
        let offer = serde_json::json!({
            "LedgerEntryType": "Offer",
            "Account": MM,
            "Sequence": 1,
            "TakerPays": iou("USD", ISSUER, "50"),
            "TakerGets": iou("EUR", ISSUER, "50"),
            "Flags": 0,
        });
        ledger
            .put_state(offer_key, serde_json::to_vec(&offer).unwrap())
            .unwrap();

        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let book_root = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
        let dir = serde_json::json!({
            "LedgerEntryType": "DirectoryNode",
            "Indexes": [offer_key.to_string()],
            "IndexNext": 0,
        });
        ledger
            .put_state(book_root, serde_json::to_vec(&dir).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("EUR", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        assert!((holder_balance(&sandbox, BOB, ISSUER, "EUR") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
    }

    // -- multi-hop walker --
    //
    // A USD→EUR→GBP payment with no direct USD/GBP book walks two book
    // crossings chained via an intermediate EUR position. `apply_cross_currency`
    // dispatches to `apply_two_hop_payment` when the transaction's `Paths`
    // field carries exactly one path with a single currency-change step
    // (`type == 0x30`). The hop sequence:
    //   * Hop 1 (USD → EUR): consume offers from `book_dir(USD, EUR)` until
    //     we've sourced enough EUR to feed the next hop.
    //   * Hop 2 (EUR → GBP): consume offers from `book_dir(EUR, GBP)` until
    //     the destination's GBP target is met.
    // Both hops must complete within `SendMax` worth of source currency or
    // the payment fails `TecPathPartial`.
    const MM2: &str = "rwUVoVMSURqNyvocPCcvLu3ygJzZyw8qwp";

    #[test]
    fn apply_cross_currency_two_hop_via_paths_succeeds() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

        // MM1: TakerPays 50 USD, TakerGets 50 EUR.
        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        // MM2: TakerPays 50 EUR, TakerGets 50 GBP.
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm2_offer = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("EUR", ISSUER, "50"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let gbp = helpers::currency_to_bytes("GBP");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let usd_eur_book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
        let eur_gbp_book = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
        ledger
            .put_state(
                usd_eur_book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                eur_gbp_book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm2_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // USD → GBP, single intermediate EUR step in Paths.
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            "Paths": [
                [
                    { "currency": "EUR", "issuer": ISSUER, "type": 0x30 }
                ]
            ],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Trust-line deltas: Alice -20 USD, MM +20 USD / -20 EUR,
        // MM2 +20 EUR / -20 GBP, Bob +20 GBP.
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "GBP") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
    }

    // -- N-hop walker (Phase 2) --

    const MM3: &str = "rMNBtf9PFe7cbij413s1CLAwejjWYB7VnR";

    /// Two MMs both publish their own intermediate-currency offers and the
    /// transaction's `Paths` field lists both as alternatives. The walker
    /// must try each in order and route through the first viable Path.
    ///
    /// Sets up:
    ///   * MM1 offers USD/EUR -> EUR/GBP (a usable 2-hop chain)
    ///   * MM2 offers USD/CHF only (a dead-end alternative; the EUR-step
    ///     book never gets crossed because CHF/GBP has no offers)
    ///
    /// Asserts that the EUR alternative wins despite being listed second.
    #[test]
    fn apply_cross_currency_two_hop_picks_viable_alternative_path() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm2_offer = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("EUR", ISSUER, "50"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let gbp = helpers::currency_to_bytes("GBP");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let usd_eur_book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
        let eur_gbp_book = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
        ledger
            .put_state(
                usd_eur_book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                eur_gbp_book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm2_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // First Path lists CHF, which has no books wired up — the walker
        // must back-solve to TecPathPartial and try the second (EUR) Path.
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            "Paths": [
                [ { "currency": "CHF", "issuer": ISSUER, "type": 0x30 } ],
                [ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]
            ],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    }

    /// Three intermediate currencies (USD->EUR->JPY->CHF->GBP, hop count 4)
    /// exercises the back-solve loop and forward mutation walk past the
    /// hard-coded two-hop unfold. Every market-maker holds full inventory
    /// of its sell-side currency, so the path is fully liquid.
    #[test]
    fn apply_cross_currency_four_hop_via_three_intermediates() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);
        put_account(&mut ledger, MM3, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "JPY", 100.0);
        put_trust_line(&mut ledger, MM3, ISSUER, "JPY", 0.0);
        put_trust_line(&mut ledger, MM3, ISSUER, "GBP", 100.0);

        let mut offer_keys = Vec::new();
        for (mm, seq, pays_cur, gets_cur) in [
            (MM, 1, "USD", "EUR"),
            (MM2, 1, "EUR", "JPY"),
            (MM3, 1, "JPY", "GBP"),
        ] {
            let mm_id = decode_account_id(mm).unwrap();
            let key = keylet::offer(&mm_id, seq);
            ledger
                .put_state(
                    key,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "Offer",
                        "Account": mm,
                        "Sequence": seq,
                        "TakerPays": iou(pays_cur, ISSUER, "50"),
                        "TakerGets": iou(gets_cur, ISSUER, "50"),
                        "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
            offer_keys.push((key, pays_cur, gets_cur));
        }
        let issuer_id = decode_account_id(ISSUER).unwrap();
        for (key, pays_cur, gets_cur) in &offer_keys {
            let pays = helpers::currency_to_bytes(pays_cur);
            let gets = helpers::currency_to_bytes(gets_cur);
            let book = keylet::book_dir(&pays, &issuer_id, &gets, &issuer_id);
            ledger
                .put_state(
                    book,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Indexes": [key.to_string()],
                        "IndexNext": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            "Paths": [[
                { "currency": "EUR", "issuer": ISSUER, "type": 0x30 },
                { "currency": "JPY", "issuer": ISSUER, "type": 0x30 }
            ]],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Trust-line deltas: ALICE -20 USD, BOB +20 GBP, each MM +20/-20.
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "JPY") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM3, ISSUER, "JPY") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM3, ISSUER, "GBP") - 80.0).abs() < 1e-6);
    }

    /// Two viable paths reach the same destination at different costs. The
    /// walker must dry-run both and commit the cheaper one (lower
    /// `src_spent`).
    ///
    /// Setup:
    ///   * Path A via EUR — MM offers USD/EUR at parity, then MM2 offers
    ///     EUR/GBP at parity. Cost to deliver 20 GBP: 20 USD.
    ///   * Path B via JPY — MM offers USD/JPY at 1:16 (16 JPY per USD) and
    ///     MM3 offers JPY/GBP at 4:1, so 5 USD buys 80 JPY which buys
    ///     20 GBP. Cost: 5 USD.
    ///
    /// First-viable-wins selection (PR #106) would pick Path A because it
    /// is listed first; the quality-ranked dispatch picks Path B.
    #[test]
    fn apply_cross_currency_picks_cheapest_path_via_quality_ranking() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);
        put_account(&mut ledger, MM3, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        // Path A: USD -> EUR -> GBP at parity (rate 1.0 throughout).
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);
        // Path B: USD -> JPY -> GBP. MM gives 4 JPY for every USD; MM3 gives
        // 1 GBP for every JPY.  Net rate USD->GBP = 4x cheaper than parity.
        put_trust_line(&mut ledger, MM, ISSUER, "JPY", 200.0);
        put_trust_line(&mut ledger, MM3, ISSUER, "JPY", 0.0);
        put_trust_line(&mut ledger, MM3, ISSUER, "GBP", 100.0);

        let issuer_id = decode_account_id(ISSUER).unwrap();
        let mm_id = decode_account_id(MM).unwrap();
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm3_id = decode_account_id(MM3).unwrap();

        // MM offer 1: USD/EUR at 1:1.
        let mm_eur = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_eur,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        // MM2 offer: EUR/GBP at 1:1.
        let mm2_gbp = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_gbp,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("EUR", ISSUER, "50"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        // MM offer 2: USD/JPY at 1:16 (16 JPY per USD).
        let mm_jpy = keylet::offer(&mm_id, 2);
        ledger
            .put_state(
                mm_jpy,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 2,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("JPY", ISSUER, "800"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        // MM3 offer: JPY/GBP at 4:1 (1 GBP per 4 JPY).
        // We want 20 GBP from MM3 in exchange for 80 JPY in.
        let mm3_gbp = keylet::offer(&mm3_id, 1);
        ledger
            .put_state(
                mm3_gbp,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM3,
                    "Sequence": 1,
                    "TakerPays": iou("JPY", ISSUER, "200"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let gbp = helpers::currency_to_bytes("GBP");
        let jpy = helpers::currency_to_bytes("JPY");

        let usd_eur = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
        let eur_gbp = keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id);
        let usd_jpy = keylet::book_dir(&usd, &issuer_id, &jpy, &issuer_id);
        let jpy_gbp = keylet::book_dir(&jpy, &issuer_id, &gbp, &issuer_id);

        for (book, offer_key) in [
            (usd_eur, mm_eur),
            (eur_gbp, mm2_gbp),
            (usd_jpy, mm_jpy),
            (jpy_gbp, mm3_gbp),
        ] {
            ledger
                .put_state(
                    book,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Indexes": [offer_key.to_string()],
                        "IndexNext": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            "Paths": [
                [ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ],
                [ { "currency": "JPY", "issuer": ISSUER, "type": 0x30 } ]
            ],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Quality-ranked dispatch picks Path B (JPY pivot).  Alice spends
        // only 5 USD; the EUR path would have cost 20 USD.
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 95.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 20.0).abs() < 1e-6);
        // JPY books were the ones consumed; EUR books should be untouched.
        assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 100.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 0.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "JPY") - 120.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM3, ISSUER, "JPY") - 80.0).abs() < 1e-6);
    }

    // -- Mixed-issuer hops (Phase 3c) --

    const ISSUER2: &str = "rJrxi4Wxev4bnAGVNP9YCdKPdAoKfAmcsi";

    /// A Path step with `type == 0x10` carries a currency change but no
    /// issuer field; the walker must inherit the issuer from the previous
    /// hop (source side here). Hop chain: USD@ISSUER -> EUR@ISSUER.
    /// Verifies the inheritance behavior on a single step.
    #[test]
    fn apply_cross_currency_inherits_issuer_from_source_side() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);

        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let book = keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id);
        ledger
            .put_state(
                book,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("EUR", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            // type == 0x10 → currency-only step; issuer is inherited from
            // the source side (ISSUER) and produces the same hop chain as
            // the 0x30 form `{ "currency": "EUR", "issuer": ISSUER }`.
            "Paths": [[ { "currency": "EUR", "type": 0x10 } ]],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "EUR") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
    }

    /// Cross-issuer chain: USD@ISSUER -> USD@ISSUER2 (issuer-only step
    /// `type == 0x20`, currency inherited from source) -> EUR@ISSUER2
    /// (final hop into the destination Amount). Exercises both inheritance
    /// directions and the `book_dir(pays_cur, pays_iss, gets_cur, gets_iss)`
    /// lookup with `pays_cur == gets_cur` but distinct issuers.
    #[test]
    fn apply_cross_currency_two_hop_mixed_issuer() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ISSUER2, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER2, "EUR", 0.0);
        // MM accepts USD@ISSUER, gives USD@ISSUER2.
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER2, "USD", 100.0);
        // MM2 accepts USD@ISSUER2, gives EUR@ISSUER2.
        put_trust_line(&mut ledger, MM2, ISSUER2, "USD", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER2, "EUR", 100.0);

        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("USD", ISSUER2, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm2_offer = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER2, "50"),
                    "TakerGets": iou("EUR", ISSUER2, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuer2_id = decode_account_id(ISSUER2).unwrap();
        let usd_usd2 = keylet::book_dir(&usd, &issuer_id, &usd, &issuer2_id);
        let usd2_eur2 = keylet::book_dir(&usd, &issuer2_id, &eur, &issuer2_id);
        ledger
            .put_state(
                usd_usd2,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                usd2_eur2,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "DirectoryNode",
                    "Indexes": [mm2_offer.to_string()],
                    "IndexNext": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("EUR", ISSUER2, "20"),
            "SendMax": iou("USD", ISSUER, "20"),
            // Single intermediate USD@ISSUER2 expressed with an issuer-only
            // step (`type == 0x20`); currency stays USD via inheritance. The
            // destination Amount (EUR@ISSUER2) closes the chain — the walker
            // appends it as the final hop boundary automatically.
            "Paths": [[
                { "issuer": ISSUER2, "type": 0x20 }
            ]],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
        assert!((holder_balance(&sandbox, BOB, ISSUER2, "EUR") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER2, "USD") - 80.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER2, "USD") - 20.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER2, "EUR") - 80.0).abs() < 1e-6);
    }

    // -- DeliverMin / tfPartialPayment (Phase 4) --

    const TF_PARTIAL_PAYMENT_TEST: u32 = 0x0002_0000;

    /// SendMax is half of what's needed to deliver the full Amount. Without
    /// `tfPartialPayment` the walker returns TecPathPartial. With the flag
    /// set and a DeliverMin <= the achievable amount, the walker scales the
    /// flow down linearly and delivers exactly half.
    #[test]
    fn apply_cross_currency_partial_payment_with_deliver_min() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);

        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm2_offer = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("EUR", ISSUER, "50"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let gbp = helpers::currency_to_bytes("GBP");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        for (book, key) in [
            (
                keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id),
                mm_offer,
            ),
            (
                keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id),
                mm2_offer,
            ),
        ] {
            ledger
                .put_state(
                    book,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Indexes": [key.to_string()],
                        "IndexNext": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        // Want 20 GBP but only willing to spend 10 USD (need 20 USD for the
        // full target). Partial mode with DeliverMin = 5 GBP allows the
        // walker to deliver 10 GBP (the max within SendMax).
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "10"),
            "DeliverMin": iou("GBP", ISSUER, "5"),
            "Flags": TF_PARTIAL_PAYMENT_TEST,
            "Paths": [[ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Alice spent SendMax (10 USD), Bob received scaled half (10 GBP).
        assert!((holder_balance(&sandbox, ALICE, ISSUER, "USD") - 90.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, BOB, ISSUER, "GBP") - 10.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "USD") - 10.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM, ISSUER, "EUR") - 90.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "EUR") - 10.0).abs() < 1e-6);
        assert!((holder_balance(&sandbox, MM2, ISSUER, "GBP") - 90.0).abs() < 1e-6);
    }

    /// Same setup as above but without the `tfPartialPayment` flag: the
    /// walker must reject the under-funded payment with TecPathPartial.
    #[test]
    fn apply_cross_currency_partial_disabled_returns_tec_path_partial() {
        let mut ledger = Ledger::genesis();
        put_account(&mut ledger, ISSUER, "100000000", None);
        put_account(&mut ledger, ALICE, "50000000", None);
        put_account(&mut ledger, BOB, "50000000", None);
        put_account(&mut ledger, MM, "50000000", None);
        put_account(&mut ledger, MM2, "50000000", None);
        put_trust_line(&mut ledger, ALICE, ISSUER, "USD", 100.0);
        put_trust_line(&mut ledger, BOB, ISSUER, "GBP", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "USD", 0.0);
        put_trust_line(&mut ledger, MM, ISSUER, "EUR", 100.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "EUR", 0.0);
        put_trust_line(&mut ledger, MM2, ISSUER, "GBP", 100.0);

        let mm_id = decode_account_id(MM).unwrap();
        let mm_offer = keylet::offer(&mm_id, 1);
        ledger
            .put_state(
                mm_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM,
                    "Sequence": 1,
                    "TakerPays": iou("USD", ISSUER, "50"),
                    "TakerGets": iou("EUR", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let mm2_id = decode_account_id(MM2).unwrap();
        let mm2_offer = keylet::offer(&mm2_id, 1);
        ledger
            .put_state(
                mm2_offer,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Offer",
                    "Account": MM2,
                    "Sequence": 1,
                    "TakerPays": iou("EUR", ISSUER, "50"),
                    "TakerGets": iou("GBP", ISSUER, "50"),
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        let usd = helpers::currency_to_bytes("USD");
        let eur = helpers::currency_to_bytes("EUR");
        let gbp = helpers::currency_to_bytes("GBP");
        let issuer_id = decode_account_id(ISSUER).unwrap();
        for (book, key) in [
            (
                keylet::book_dir(&usd, &issuer_id, &eur, &issuer_id),
                mm_offer,
            ),
            (
                keylet::book_dir(&eur, &issuer_id, &gbp, &issuer_id),
                mm2_offer,
            ),
        ] {
            ledger
                .put_state(
                    book,
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "DirectoryNode",
                        "Indexes": [key.to_string()],
                        "IndexNext": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": ALICE,
            "Destination": BOB,
            "Amount": iou("GBP", ISSUER, "20"),
            "SendMax": iou("USD", ISSUER, "10"),
            // No DeliverMin, no tfPartialPayment.
            "Paths": [[ { "currency": "EUR", "issuer": ISSUER, "type": 0x30 } ]],
            "Fee": "10",
        });
        let rules = Rules::new();
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TecPathPartial));
    }
}
