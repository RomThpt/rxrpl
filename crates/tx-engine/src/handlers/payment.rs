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
            // Cross-currency: a SendMax in a different currency, or one naming a
            // third-party issuer, means the payment must flow through the order
            // book. A same-currency SendMax whose issuer is the sender itself is
            // the "spend from my own holdings" convention (e.g. redeeming an IOU
            // back to its issuer), which the direct IOU path handles.
            if let Some((sm_cur, sm_iss, sm_val)) = get_send_max_iou(ctx.tx) {
                if sm_cur != currency || (sm_iss != issuer && sm_iss != account_str) {
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
        crate::owner_dir::consume_seq_or_ticket(ctx.view, &src_id, &mut src_account, ctx.tx)?;

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
    crate::owner_dir::consume_seq_or_ticket(ctx.view, &src_id, &mut src_acct, ctx.tx)?;
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
    crate::owner_dir::consume_seq_or_ticket(ctx.view, &src_id, &mut src_acct, ctx.tx)?;
    let src_acct_data =
        serde_json::to_vec(&src_acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_key, src_acct_data)
        .map_err(|_| TransactionResult::TefInternal)?;

    Ok(TransactionResult::TesSuccess)
}

/// A resolved Path step expressed as a hop boundary plus its transit kind.
///
/// `Book` boundaries are crossed via the order book between the previous
/// `(cur, iss)` and the new `(cur, iss)`. `Ripple` boundaries are crossed
/// by transiting through `account`, which absorbs the inbound IOU (same
/// currency, previous issuer) and re-issues it as its own.
enum PathHopSpec {
    Book { cur: String, iss: String },
    Ripple { cur: String, account: String },
}

/// Inspect a single Path (array of step objects) and resolve each step's
/// effective hop boundary by walking the inheritance chain from the source
/// side. Returns the fully resolved chain of intermediates in walk order,
/// or `None` if any step carries an unsupported combination of bits.
///
/// rippled's PathStep bitmap:
///   * `0x01` (PATH_STEP_ACCOUNT): account-rippling through `account`
///   * `0x10` (PATH_STEP_CURRENCY): currency comes from the step
///   * `0x20` (PATH_STEP_ISSUER): issuer comes from the step
///   * `0x30` (combined): both come from the step
///
/// Supported bit shapes:
///   * pure book: `0x10` / `0x20` / `0x30`
///   * pure rippling: `0x01`
///
/// `0x11` / `0x21` / `0x31` (account combined with currency or issuer)
/// are intentionally unsupported and fall back to alternative Paths.
///
/// For a `0x10`-only step the issuer is inherited from the previous hop
/// (or the source side for the first step). For a `0x20`-only step the
/// currency is inherited the same way. A `0x01` step keeps the currency
/// and changes the issuer to `account`.
fn simple_path_intermediates(
    path: &Vec<serde_json::Value>,
    src_cur: &str,
    src_iss: &str,
) -> Option<Vec<PathHopSpec>> {
    const PATH_STEP_ACCOUNT: u64 = 0x01;
    const PATH_STEP_CURRENCY: u64 = 0x10;
    const PATH_STEP_ISSUER: u64 = 0x20;

    let mut out = Vec::with_capacity(path.len());
    let mut current_cur = src_cur.to_string();
    let mut current_iss = src_iss.to_string();
    for step in path {
        let step_type = step.get("type").and_then(|v| v.as_u64()).unwrap_or(0);
        let has_account = step_type & PATH_STEP_ACCOUNT != 0;
        let has_currency = step_type & PATH_STEP_CURRENCY != 0;
        let has_issuer = step_type & PATH_STEP_ISSUER != 0;
        if has_account {
            // Mixed account+currency/issuer steps are deferred; only pure
            // account-rippling (0x01) is supported here.
            if has_currency || has_issuer {
                return None;
            }
            let account = step.get("account").and_then(|v| v.as_str())?.to_string();
            current_iss = account.clone();
            out.push(PathHopSpec::Ripple {
                cur: current_cur.clone(),
                account,
            });
            continue;
        }
        if !has_currency && !has_issuer {
            return None;
        }
        if has_currency {
            current_cur = step.get("currency").and_then(|v| v.as_str())?.to_string();
        }
        if has_issuer {
            current_iss = step.get("issuer").and_then(|v| v.as_str())?.to_string();
        }
        out.push(PathHopSpec::Book {
            cur: current_cur.clone(),
            iss: current_iss.clone(),
        });
    }
    Some(out)
}

/// AMM swap consumed during a Book hop's back-solve. The remaining
/// required output not satisfied by the order book is routed through
/// the AMM via constant-product math.
struct AmmConsume {
    amm_key: rxrpl_primitives::Hash256,
    in_amount: f64,
    out_amount: f64,
    /// `true` when the inbound currency matches PoolBalance1 in the AMM
    /// SLE, `false` when it matches PoolBalance2. Drives which side of
    /// the pool gets incremented vs decremented at commit time.
    in_is_pool1: bool,
}

/// Per-hop application kind. `Book` hops cross an order book and consume
/// a list of offers (plus an optional AMM swap for the residual output);
/// `Ripple` hops transit through `account`, which takes the inbound IOU
/// into its trust line with the previous issuer.
enum HopAction {
    Book {
        book: rxrpl_primitives::Hash256,
        consumed: Vec<(
            rxrpl_primitives::Hash256,
            rxrpl_primitives::AccountId,
            f64,
            f64,
        )>,
        amm: Option<AmmConsume>,
    },
    Ripple {
        account: rxrpl_primitives::AccountId,
        amount: f64,
    },
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
    /// Per-hop application, `hops.len() == chain.len() - 1`.
    hops: Vec<HopAction>,
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
#[allow(clippy::too_many_arguments)]
fn back_solve_n_hop(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: (&str, &str, &str),
    send_max: (&str, &str, &str),
    intermediates: &[PathHopSpec],
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

    // Build the (currency, issuer) chain along with the per-step kind:
    // src -> step1 -> ... -> stepN. If the last step doesn't land on the
    // destination boundary, an implicit final book hop is appended.
    let mut chain: Vec<([u8; 20], rxrpl_primitives::AccountId)> = Vec::new();
    chain.push((
        helpers::currency_to_bytes(src_cur),
        decode_account_id(src_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
    ));
    // Per-step kind tracking lets back-solve pick the right transit rule.
    // None marks a Book hop; Some(account) marks a Ripple hop through
    // that account. Indexed alongside `chain[1..]`.
    let mut step_kinds: Vec<Option<rxrpl_primitives::AccountId>> = Vec::new();
    for spec in intermediates {
        match spec {
            PathHopSpec::Book { cur, iss } => {
                chain.push((
                    helpers::currency_to_bytes(cur),
                    decode_account_id(iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
                ));
                step_kinds.push(None);
            }
            PathHopSpec::Ripple { cur, account } => {
                let account_id = decode_account_id(account)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                chain.push((helpers::currency_to_bytes(cur), account_id));
                step_kinds.push(Some(account_id));
            }
        }
    }
    let dst_boundary = (
        helpers::currency_to_bytes(dst_cur),
        decode_account_id(dst_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?,
    );
    if chain.last() != Some(&dst_boundary) {
        chain.push(dst_boundary);
        step_kinds.push(None);
    }
    let hop_count = chain.len() - 1;
    if hop_count == 0 {
        return Err(TransactionResult::TecPathPartial);
    }

    let mut hops: Vec<Option<HopAction>> = (0..hop_count).map(|_| None).collect();

    let mut required_output = target;
    for hop in (0..hop_count).rev() {
        let (in_cur, in_iss) = &chain[hop];
        let (out_cur, out_iss) = &chain[hop + 1];

        if let Some(account) = step_kinds[hop] {
            // Same-currency rippling transit: no exchange, amount through.
            // The ripple account must already hold a trust line with the
            // inbound issuer; if it doesn't, fail this path so the
            // dispatcher tries an alternative.
            let tl_key = keylet::trust_line(&account, in_iss, in_cur);
            if ctx.view.read(&tl_key).is_none() {
                return Err(TransactionResult::TecPathDry);
            }
            hops[hop] = Some(HopAction::Ripple {
                account,
                amount: required_output,
            });
            continue;
        }

        let book = keylet::book_dir(in_cur, in_iss, out_cur, out_iss);
        let offers = collect_book_offers(ctx, &book);

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

        // Residual liquidity from the AMM, when one is registered for the
        // (in, out) pair and the book ran out before satisfying
        // `required_output`. The swap input feeds back into `input_required`
        // so the upstream hop sees the AMM cost as part of the source spend.
        let mut amm: Option<AmmConsume> = None;
        if remaining > 1e-9 {
            if let Some(quote) = quote_amm_swap(ctx, in_cur, in_iss, out_cur, out_iss, remaining) {
                input_required += quote.in_amount;
                remaining = 0.0;
                amm = Some(quote);
            }
        }

        if remaining > 1e-9 {
            return Err(TransactionResult::TecPathPartial);
        }
        hops[hop] = Some(HopAction::Book {
            book,
            consumed,
            amm,
        });
        required_output = input_required;
    }
    let mut hops: Vec<HopAction> = hops.into_iter().map(|h| h.expect("hop filled")).collect();

    let src_spent_full = required_output;
    if src_spent_full <= send_max_val + 1e-9 {
        return Ok(NHopPlan {
            src_id,
            dest_id,
            target,
            src_spent: src_spent_full,
            chain,
            hops,
        });
    }

    // Source spend exceeds SendMax. Without partial mode, fail. With it,
    // scale all consumption proportionally so the actual delivered amount
    // is the maximum reachable within SendMax. The flow is linear in each
    // hop (offer slices and ripple-hop amounts are taken proportionally
    // to the desired output), so a uniform scale factor preserves all
    // per-offer and per-ripple ratios.
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
    // Strands that touch the AMM cannot be scaled uniformly: the swap is
    // non-linear (constant product), so a single `scale` factor diverges
    // from the actual on-chain quote. Instead bisect on the deliverable
    // target — source cost is monotonically increasing in target — and
    // rebuild the plan at the largest target whose cost fits SendMax.
    // `back_solve_n_hop` is read-only, so re-invoking it is side-effect
    // free; `partial_allowed=false` on the recursive calls makes them
    // return the full plan when it fits and `tecPATH_PARTIAL` otherwise,
    // which is exactly the feasibility predicate we bisect on.
    let touches_amm = hops
        .iter()
        .any(|a| matches!(a, HopAction::Book { amm: Some(_), .. }));
    if touches_amm {
        let mut lo = 0.0f64;
        let mut hi = target;
        // 64 halvings drives the f64 interval well below any representable
        // amount; cost monotonicity guarantees convergence to the SendMax tip.
        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            let mid_s = format!("{mid}");
            match back_solve_n_hop(
                ctx,
                account_str,
                destination_str,
                (dst_cur, dst_iss, &mid_s),
                send_max,
                intermediates,
                false,
                None,
            ) {
                Ok(p) if p.src_spent <= send_max_val + 1e-9 => lo = mid,
                _ => hi = mid,
            }
        }
        if lo <= 0.0 {
            return Err(TransactionResult::TecPathPartial);
        }
        if let Some(d_min) = deliver_min {
            if lo + 1e-9 < d_min {
                return Err(TransactionResult::TecPathPartial);
            }
        }
        let lo_s = format!("{lo}");
        return back_solve_n_hop(
            ctx,
            account_str,
            destination_str,
            (dst_cur, dst_iss, &lo_s),
            send_max,
            intermediates,
            false,
            None,
        );
    }
    for action in hops.iter_mut() {
        match action {
            HopAction::Book { consumed, .. } => {
                for entry in consumed.iter_mut() {
                    entry.2 *= scale;
                    entry.3 *= scale;
                }
            }
            HopAction::Ripple { amount, .. } => {
                *amount *= scale;
            }
        }
    }

    Ok(NHopPlan {
        src_id,
        dest_id,
        target: scaled_target,
        src_spent: send_max_val,
        chain,
        hops,
    })
}

/// Back-solve a single-leg AMM swap for the given hop pair. Returns
/// `Some(AmmConsume)` when an AMM SLE exists for `(in, out)` and has
/// enough liquidity to deliver `out_take`; otherwise `None`.
///
/// Pricing follows rippled's constant-product convention with the
/// trading fee taken from the input:
///
///   effective_in = input * (1 - fee_bps / 100_000)
///   (pool_in + effective_in) * (pool_out - out_take) = pool_in * pool_out
///
/// Solving for `input` gives `out_take`'s required quote.
fn quote_amm_swap(
    ctx: &mut ApplyContext<'_>,
    in_cur: &[u8; 20],
    in_iss: &rxrpl_primitives::AccountId,
    out_cur: &[u8; 20],
    out_iss: &rxrpl_primitives::AccountId,
    out_take: f64,
) -> Option<AmmConsume> {
    if out_take <= 1e-9 {
        return None;
    }
    let amm_key = rxrpl_protocol::keylet::amm(in_cur, in_iss, out_cur, out_iss);
    let amm_bytes = ctx.view.read(&amm_key)?;
    let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).ok()?;

    let pool1 = crate::amm_helpers::get_pool_field(&amm, "PoolBalance1") as f64;
    let pool2 = crate::amm_helpers::get_pool_field(&amm, "PoolBalance2") as f64;
    if pool1 <= 0.0 || pool2 <= 0.0 {
        return None;
    }

    // The AMM keylet sorts its two assets canonically (see
    // `amm_helpers::sort_assets`). PoolBalance1 corresponds to the
    // lexicographically-smaller `(currency, issuer)` tuple. Match the
    // incoming currency against that ordering to pick the right side.
    let in_key = (in_cur.as_slice(), in_iss.as_bytes());
    let out_key = (out_cur.as_slice(), out_iss.as_bytes());
    let in_is_pool1 = in_key <= out_key;
    let (pool_in, pool_out) = if in_is_pool1 {
        (pool1, pool2)
    } else {
        (pool2, pool1)
    };
    if out_take >= pool_out {
        // Cannot withdraw the entire pool; rippled rejects this as
        // insufficient liquidity.
        return None;
    }

    let fee_bps = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as f64;
    let fee_fraction = (fee_bps / 100_000.0).clamp(0.0, 0.999);
    let new_pool_out = pool_out - out_take;
    let effective_in = pool_in * pool_out / new_pool_out - pool_in;
    let in_amount = effective_in / (1.0 - fee_fraction);
    if !in_amount.is_finite() || in_amount <= 0.0 {
        return None;
    }

    Some(AmmConsume {
        amm_key,
        in_amount,
        out_amount: out_take,
        in_is_pool1,
    })
}

/// Commit the AMM-side of a Book hop: update the SLE's `PoolBalance1`
/// and `PoolBalance2` fields to reflect the swap. Mirrors the
/// `amm_deposit` handler's policy of touching only the on-SLE pool
/// fields and skipping the pseudo-account's trust-line balances.
fn apply_amm_swap(ctx: &mut ApplyContext<'_>, swap: &AmmConsume) -> Result<(), TransactionResult> {
    let bytes = ctx
        .view
        .read(&swap.amm_key)
        .ok_or(TransactionResult::TecNoEntry)?;
    let mut amm: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let pool1 = crate::amm_helpers::get_pool_field(&amm, "PoolBalance1") as f64;
    let pool2 = crate::amm_helpers::get_pool_field(&amm, "PoolBalance2") as f64;
    let (new_pool1, new_pool2) = if swap.in_is_pool1 {
        (pool1 + swap.in_amount, pool2 - swap.out_amount)
    } else {
        (pool1 - swap.out_amount, pool2 + swap.in_amount)
    };
    if new_pool1 < 0.0 || new_pool2 < 0.0 {
        return Err(TransactionResult::TecPathPartial);
    }
    amm["PoolBalance1"] = serde_json::Value::String((new_pool1 as u64).to_string());
    amm["PoolBalance2"] = serde_json::Value::String((new_pool2 as u64).to_string());
    let data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(swap.amm_key, data)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
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
        hops,
    } = plan;
    let hop_count = chain.len() - 1;
    debug_assert_eq!(hops.len(), hop_count);

    let (first_cur, first_iss) = &chain[0];
    apply_trust_delta(ctx, &src_id, first_iss, first_cur, -src_spent)?;

    for (hop, action) in hops.iter().enumerate() {
        let (in_cur, in_iss) = &chain[hop];
        let (out_cur, out_iss) = &chain[hop + 1];
        match action {
            HopAction::Book {
                book,
                consumed,
                amm,
            } => {
                for (offer_key, owner_id, take_in, take_out) in consumed {
                    if *owner_id != *in_iss {
                        apply_trust_delta(ctx, owner_id, in_iss, in_cur, *take_in)?;
                    }
                    if *owner_id != *out_iss {
                        apply_trust_delta(ctx, owner_id, out_iss, out_cur, -*take_out)?;
                    }
                    update_consumed_offer(ctx, offer_key, book, *take_in, *take_out)?;
                }
                if let Some(swap) = amm {
                    apply_amm_swap(ctx, swap)?;
                }
            }
            HopAction::Ripple { account, amount } => {
                // Ripple-through: account absorbs the inbound IOU into its
                // trust line with `in_iss`. The corresponding outflow as
                // IOU issued by `account` is realised by the next hop's
                // payer (book offer owner, next ripple account, or the
                // destination), so we don't credit `account` against
                // itself here.
                apply_trust_delta(ctx, account, in_iss, in_cur, *amount)?;
            }
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
    crate::owner_dir::consume_seq_or_ticket(ctx.view, &src_id, &mut src_acct, ctx.tx)?;
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
mod tests;
