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
    // Multi-hop dispatch: a single Path with one intermediate currency step
    // (e.g. USD->EUR->GBP) is routed through `apply_two_hop_payment`. More
    // complex shapes (multiple intermediate steps, rippling through accounts,
    // mixed issuers) still fall through to the direct-book code path below.
    if let Some(intermediate) = first_simple_intermediate(ctx.tx) {
        return apply_two_hop_payment(
            ctx,
            account_str,
            destination_str,
            amount,
            send_max,
            intermediate,
        );
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

/// Inspect the transaction's `Paths` field and, if it contains a single Path
/// with exactly one currency-change step, return `(currency, issuer)` of the
/// intermediate pivot. Returns `None` for any shape we don't yet handle:
/// missing `Paths`, multiple paths, account-rippling steps, multi-step paths,
/// or unparseable entries. Mirrors rippled's PathStep bitmap: 0x10 = currency
/// change, 0x20 = issuer change.
fn first_simple_intermediate(tx: &serde_json::Value) -> Option<(String, String)> {
    let paths = tx.get("Paths").and_then(|v| v.as_array())?;
    if paths.len() != 1 {
        return None;
    }
    let path = paths.first()?.as_array()?;
    if path.len() != 1 {
        return None;
    }
    let step = path.first()?;
    let step_type = step.get("type").and_then(|v| v.as_u64()).unwrap_or(0);
    // Require currency-change (0x10) + issuer (0x20); reject pure-account or
    // unknown bits so a future implementation pass extends scope cleanly.
    if step_type != 0x30 {
        return None;
    }
    let currency = step.get("currency").and_then(|v| v.as_str())?.to_string();
    let issuer = step.get("issuer").and_then(|v| v.as_str())?.to_string();
    Some((currency, issuer))
}

/// Apply a two-hop cross-currency Payment: source pays `send_max` (currency
/// A), routes through an intermediate `(int_cur, int_iss)`, and delivers
/// `amount` (currency B). Two books are crossed: A/int and int/B. Both must
/// hold enough offers to cover the target Amount within send_max. Trust-line
/// debits/credits, offer mutations and the source `Sequence` bump mirror the
/// single-hop `apply_cross_currency` flow.
fn apply_two_hop_payment(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: (&str, &str, &str),
    send_max: (&str, &str, &str),
    intermediate: (String, String),
) -> Result<TransactionResult, TransactionResult> {
    let (dst_cur, dst_iss, dst_val) = amount;
    let (src_cur, src_iss, src_max) = send_max;
    let (int_cur, int_iss) = intermediate;

    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dest_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dst_issuer_id =
        decode_account_id(dst_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let src_issuer_id =
        decode_account_id(src_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let int_issuer_id =
        decode_account_id(&int_iss).map_err(|_| TransactionResult::TemInvalidAccountId)?;

    let dst_cur_bytes = helpers::currency_to_bytes(dst_cur);
    let src_cur_bytes = helpers::currency_to_bytes(src_cur);
    let int_cur_bytes = helpers::currency_to_bytes(&int_cur);

    let target: f64 = dst_val
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    let send_max_val: f64 = src_max
        .parse()
        .map_err(|_| TransactionResult::TemBadAmount)?;
    if target <= 0.0 {
        return Err(TransactionResult::TemBadAmount);
    }

    // Hop 2 (int -> dst): book keyed by pays = int, gets = dst. Offers are
    // selling dst for int. We consume in quality order to back-solve how
    // much intermediate currency we need to deliver the target.
    let hop2_book = keylet::book_dir(
        &int_cur_bytes,
        &int_issuer_id,
        &dst_cur_bytes,
        &dst_issuer_id,
    );
    let hop2_offers = collect_book_offers(ctx, &hop2_book);

    let mut hop2_remaining = target;
    let mut hop2_consumed: Vec<(
        rxrpl_primitives::Hash256,
        rxrpl_primitives::AccountId,
        f64,
        f64,
    )> = Vec::new();
    let mut intermediate_required = 0.0;
    for offer in &hop2_offers {
        if hop2_remaining <= 1e-9 {
            break;
        }
        if offer.taker_gets <= 0.0 || offer.taker_pays <= 0.0 {
            continue;
        }
        let take_dst = hop2_remaining.min(offer.taker_gets);
        let take_int = take_dst * offer.taker_pays / offer.taker_gets;
        hop2_consumed.push((offer.key, offer.owner, take_int, take_dst));
        hop2_remaining -= take_dst;
        intermediate_required += take_int;
    }
    if hop2_remaining > 1e-9 {
        return Err(TransactionResult::TecPathPartial);
    }

    // Hop 1 (src -> int): book keyed by pays = src, gets = int. Offers sell
    // int for src. Consume just enough to produce `intermediate_required`.
    let hop1_book = keylet::book_dir(
        &src_cur_bytes,
        &src_issuer_id,
        &int_cur_bytes,
        &int_issuer_id,
    );
    let hop1_offers = collect_book_offers(ctx, &hop1_book);

    let mut hop1_remaining = intermediate_required;
    let mut hop1_consumed: Vec<(
        rxrpl_primitives::Hash256,
        rxrpl_primitives::AccountId,
        f64,
        f64,
    )> = Vec::new();
    let mut src_spent = 0.0;
    for offer in &hop1_offers {
        if hop1_remaining <= 1e-9 {
            break;
        }
        if offer.taker_gets <= 0.0 || offer.taker_pays <= 0.0 {
            continue;
        }
        let take_int = hop1_remaining.min(offer.taker_gets);
        let take_src = take_int * offer.taker_pays / offer.taker_gets;
        if src_spent + take_src > send_max_val + 1e-9 {
            return Err(TransactionResult::TecPathPartial);
        }
        hop1_consumed.push((offer.key, offer.owner, take_src, take_int));
        hop1_remaining -= take_int;
        src_spent += take_src;
    }
    if hop1_remaining > 1e-9 {
        return Err(TransactionResult::TecPathPartial);
    }

    // Apply mutations. All-or-nothing within the sandbox: any error from
    // here on returns an internal failure code and rolls back via the
    // outer sandbox commit/abort logic.
    apply_trust_delta(ctx, &src_id, &src_issuer_id, &src_cur_bytes, -src_spent)?;

    for (offer_key, owner_id, take_src, take_int) in &hop1_consumed {
        if *owner_id != src_issuer_id {
            apply_trust_delta(ctx, owner_id, &src_issuer_id, &src_cur_bytes, *take_src)?;
        }
        if *owner_id != int_issuer_id {
            apply_trust_delta(ctx, owner_id, &int_issuer_id, &int_cur_bytes, -*take_int)?;
        }
        update_consumed_offer(ctx, offer_key, &hop1_book, *take_src, *take_int)?;
    }

    for (offer_key, owner_id, take_int, take_dst) in &hop2_consumed {
        if *owner_id != int_issuer_id {
            apply_trust_delta(ctx, owner_id, &int_issuer_id, &int_cur_bytes, *take_int)?;
        }
        if *owner_id != dst_issuer_id {
            apply_trust_delta(ctx, owner_id, &dst_issuer_id, &dst_cur_bytes, -*take_dst)?;
        }
        update_consumed_offer(ctx, offer_key, &hop2_book, *take_int, *take_dst)?;
    }

    apply_trust_delta(ctx, &dest_id, &dst_issuer_id, &dst_cur_bytes, target)?;

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
}
