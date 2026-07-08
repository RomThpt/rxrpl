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
            // A payment to self is only valid as a cross-currency conversion:
            // the Amount must name a different asset than SendMax. rippled
            // rejects a same-asset self-send as redundant.
            let conversion = ctx
                .tx
                .get("SendMax")
                .map(|sm| cross_assets_differ(sm, &ctx.tx["Amount"]))
                .unwrap_or(false);
            if !conversion {
                return Err(TransactionResult::TemBadSend);
            }
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

        // NOTE: DepositAuth (tecNO_PERMISSION) and RequireDestTag
        // (tecDST_TAG_NEEDED) are tec-returning checks; rippled claims a tec
        // (fee + seq charged). Since the engine now consumes fee + seq centrally
        // in the parent sandbox *before* doApply, these checks were moved to the
        // start of `Payment::apply` so they land as apply-tecs (kept on tec)
        // rather than free preclaim returns.

        // Cross-currency payment: the source spends SendMax (a different asset),
        // not Amount, so the Amount-vs-balance funding check below does not
        // apply — the crossing in apply enforces source funds.
        if let Some(sm) = ctx.tx.get("SendMax") {
            if cross_assets_differ(sm, &ctx.tx["Amount"]) {
                return Ok(());
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

        // DepositAuth / RequireDestTag: these return a `tec` that rippled claims
        // (fee + seq charged). The engine consumes fee + seq centrally in the
        // parent sandbox *before* doApply, so these checks run here at the start
        // of apply (instead of preclaim) to land as apply-tecs that survive on
        // tec. Mirrors rippled checking them in preclaim while still claiming.
        {
            let dst_id = decode_account_id(destination_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dst_key = keylet::account(&dst_id);
            if let Some(bytes) = ctx.view.read(&dst_key) {
                if let Ok(dst_account) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    let dst_flags = dst_account
                        .get("Flags")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32;
                    const LSF_DEPOSIT_AUTH: u32 = 0x01000000;
                    const LSF_REQUIRE_DEST_TAG: u32 = 0x00020000;
                    if dst_flags & LSF_DEPOSIT_AUTH != 0 && account_str != destination_str {
                        let src_id = decode_account_id(account_str)
                            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
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
        }

        // Cross-currency Payment (a SendMax in a different asset than Amount):
        // cross the order book via the shared Taker engine — for a conversion
        // (Account == Destination) or a direct payment to another account.
        if let Some(send_max) = ctx.tx.get("SendMax") {
            if cross_assets_differ(send_max, &ctx.tx["Amount"]) {
                // Multi-path Flow: a Payment with two or more alternative `Paths`
                // that each resolve to a pure book/AMM boundary chain. rippled
                // runs these through the multi-pass Flow loop, consuming any
                // shared AMM pool in fibonacci-sized chunks clamped to the
                // competing CLOB quality; a single full swap over-delivers. Gated
                // narrowly (>= 2 resolvable boundary chains) so single-path
                // cross-currency and AMM-routed payments keep their existing
                // byte-exact single-strand engine.
                if count_flow_strands(ctx.view, ctx.tx, send_max, &ctx.tx["Amount"]) >= 2
                    && flow_strands_have_amm(ctx.view, ctx.tx, send_max, &ctx.tx["Amount"])
                {
                    return apply_paths_payment_multi(
                        ctx,
                        account_str,
                        destination_str,
                        ctx.tx["Amount"].clone(),
                        send_max.clone(),
                    );
                }
                if ctx.tx.get("Paths").is_none() {
                    // Single-book shape.
                    return apply_conversion(
                        ctx,
                        account_str,
                        destination_str,
                        ctx.tx["Amount"].clone(),
                        send_max.clone(),
                    );
                }
                // Multi-hop `Paths` with a native (XRP) source or destination
                // leg: the legacy IOU-only back-solve below cannot represent the
                // native leg, so route to the byte-exact book-chain crossing
                // built on the shared Taker engine. IOU<->IOU multi-hop keeps its
                // existing dispatch (apply_cross_currency) further down.
                if send_max.is_string() || ctx.tx["Amount"].is_string() {
                    return apply_paths_payment(
                        ctx,
                        account_str,
                        destination_str,
                        ctx.tx["Amount"].clone(),
                        send_max.clone(),
                    );
                }
                // IOU<->IOU multi-hop `Paths`: when at least one path resolves to
                // a pure book/AMM chain (no genuine cross-issuer DirectStep), route
                // it through the same byte-exact engine as the native-leg case.
                // Paths that need a genuine DirectStep or multi-path blend the
                // book/AMM chain cannot represent fall through to the legacy
                // back-solve (`apply_cross_currency`) below.
                if paths_resolve_to_chain(ctx.view, ctx.tx, send_max, &ctx.tx["Amount"]) {
                    return apply_paths_payment(
                        ctx,
                        account_str,
                        destination_str,
                        ctx.tx["Amount"].clone(),
                        send_max.clone(),
                    );
                }
            }
        }

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
    compute_new_iou_balance(trust, delta_str, issuer_id, holder_id)
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

/// The (currency, issuer) asset identity of an Amount value. XRP (a bare drops
/// string) maps to the sentinel `("XRP", "")`.
fn asset_of(v: &serde_json::Value) -> (String, String) {
    if v.is_string() {
        return ("XRP".into(), String::new());
    }
    let cur = v.get("currency").and_then(|c| c.as_str()).unwrap_or("");
    let iss = v.get("issuer").and_then(|c| c.as_str()).unwrap_or("");
    (cur.to_string(), iss.to_string())
}

/// True when two Amount values name different assets (the payment must cross a
/// book to convert one into the other).
fn cross_assets_differ(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    asset_of(a) != asset_of(b)
}

/// Apply a cross-currency conversion Payment (`Account == Destination`): cross
/// the order book with the shared Taker engine, delivering up to `amount` while
/// spending at most `send_max`. The transaction fee is already charged on the
/// account by the engine; this reads that working copy, consumes the sequence,
/// applies the crossing mutations, and writes it back.
fn apply_conversion(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: serde_json::Value,
    send_max: serde_json::Value,
) -> Result<TransactionResult, TransactionResult> {
    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dst_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let src_key = keylet::account(&src_id);
    let bytes = ctx
        .view
        .read(&src_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;

    // AMM-routed conversion: when an AMM pool exists for the (SendMax → Amount)
    // pair, swap through it with exact constant-product math. The pool here is
    // always the sole/best liquidity for the books we replay (the swap is small
    // relative to the reserves, so its quality beats any resting offer and
    // rippled never reaches the CLOB); for those it either spends the whole
    // budget or delivers the whole target, so the order book is left untouched.
    // Books with no AMM (every pre-AMM order-book oracle) fall through to the
    // shared Taker engine unchanged.
    let delivered = match try_amm_conversion(ctx, &src_id, &mut acct, &amount, &send_max)? {
        Some(delivered) => delivered,
        None => {
            crate::handlers::offer_create::cross_book_payment(
                ctx, &src_id, &mut acct, &dst_id, &amount, &send_max,
            )?
            .0
        }
    };

    // rippled: a cross-currency payment that delivers exactly zero is dry and
    // returns tecPATH_DRY EVEN under tfPartialPayment (StrandFlow.h:803-806) —
    // only a strictly positive (even dust) delivery is tesSUCCESS. Returning it
    // as a retryable tec lets the build-ledger loop defer the payment to a later
    // pass once a sibling funds its path, instead of committing a no-op success.
    if delivered_amount(&delivered) <= 0.0 {
        return Err(TransactionResult::TecPathDry);
    }

    const TF_PARTIAL_PAYMENT: u32 = rxrpl_protocol::flags::payment::TF_PARTIAL_PAYMENT;
    let partial = helpers::get_flags(ctx.tx) & TF_PARTIAL_PAYMENT != 0;
    if !partial && !delivered_meets_target(&delivered, &amount) {
        return Err(TransactionResult::TecPathPartial);
    }

    // rippled Payment::doApply: record sfDeliveredAmount when the delivered
    // amount differs from the requested Amount (a partial or path-limited
    // delivery); a full delivery leaves it unset.
    if delivered != amount {
        ctx.view.set_delivered_amount(delivered.clone());
    }

    let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(TransactionResult::TesSuccess)
}

/// Whether at least one of the transaction's `Paths` is a real
/// pathfinder-shaped pure book/AMM chain that the byte-exact engine can serve.
///
/// rippled's pathfinder threads every interior book step with an account-ripple
/// step (`type` 0x01) through that book's output issuer (e.g. `book RPR/I`,
/// `account I`, `book FUZZY/J`, `account J`); `build_path_boundaries` folds those
/// no-op ripple steps away to a book/AMM boundary chain. Requiring such a step
/// keeps the byte-exact engine to the mainnet path shape (Gap 2) and leaves the
/// legacy back-solve to serve the bare-book synthetic shapes and any path that
/// needs a genuine cross-issuer `DirectStep` or multi-path quality blend.
fn paths_resolve_to_chain(
    view: &dyn ReadView,
    tx: &serde_json::Value,
    send_max: &serde_json::Value,
    amount: &serde_json::Value,
) -> bool {
    tx.get("Paths")
        .and_then(|v| v.as_array())
        .map(|paths| {
            paths.iter().any(|p| {
                let Some(steps) = p.as_array() else {
                    return false;
                };
                let has_ripple_step = steps
                    .iter()
                    .any(|s| s.get("account").and_then(|v| v.as_str()).is_some());
                has_ripple_step
                    && build_path_boundaries(view, steps, send_max, amount)
                        .map(|b| b.len() >= 2)
                        .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Whether `account`'s `AccountRoot` carries an `AMMID` field — i.e. it is an
/// AMM pseudo-account. A bare account path step naming such an account is an AMM
/// crossing pivot (folded into the surrounding book/AMM-by-pair hop) rather than
/// a genuine cross-issuer `DirectStep` ripple.
fn account_has_ammid(view: &dyn ReadView, account: &str) -> bool {
    let Ok(id) = decode_account_id(account) else {
        return false;
    };
    let Some(bytes) = view.read(&keylet::account(&id)) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|a| a.get("AMMID").cloned())
        .is_some()
}

/// Build the boundary chain of a single resolved path: `[source asset,
/// intermediate1, intermediate2, ..., destination asset]`. Each interior
/// boundary is an IOU object `{currency, issuer, value:"0"}` (the magnitude is
/// supplied by the crossing). Returns `None` for any path step that is not a
/// pure book step (an account-ripple `0x01` step or a step that names neither a
/// currency nor an issuer), so the caller can fall back to another path.
fn build_path_boundaries(
    view: &dyn ReadView,
    path: &[serde_json::Value],
    send_max: &serde_json::Value,
    amount: &serde_json::Value,
) -> Option<Vec<serde_json::Value>> {
    let boundary_asset = |v: &serde_json::Value| -> (String, String) { asset_of(v) };

    let mut out = vec![send_max.clone()];
    let (mut cur, mut iss) = boundary_asset(send_max);
    for step in path {
        // rippled serializes a PathStep as the present subset of
        // account/currency/issuer (the `type` bitmap is derived from which
        // fields appear and is NOT emitted by the binary decoder). Infer the
        // step kind from field presence:
        //   * an `account` step is account-rippling (not a pure book step) and
        //     is not modelled by this handler — fall back to another path.
        //   * a `currency`/`issuer` step changes the book boundary; the omitted
        //     side is inherited from the previous hop.
        let step_account = step.get("account").and_then(|v| v.as_str());
        let step_cur = step.get("currency").and_then(|v| v.as_str());
        let step_iss = step.get("issuer").and_then(|v| v.as_str());
        if let Some(account) = step_account {
            // A bare account-rippling step (`type` 0x01, no currency/issuer)
            // ripples into that account keeping the current asset; whether it
            // changes the boundary depends on what that account is:
            //   * `account == iss` is a literal no-op (the book output lands on
            //     the issuer's own books before the next book step). Skip it.
            //   * an AMM pseudo-account (its `AccountRoot` carries `AMMID`) is the
            //     pivot the pathfinder names for an AMM crossing: the currency
            //     change and liquidity are resolved between the adjacent
            //     boundaries by `cross_path_payment` (AMM-by-pair via
            //     `keylet::amm`), so the step is a pass-through here.
            //   * any other account is a genuine cross-issuer `DirectStep` ripple
            //     (same currency, different issuer obligations) that this
            //     book/AMM-chain engine does not model — fall back to another path
            //     (or the legacy IOU<->IOU back-solve).
            if step_cur.is_none()
                && step_iss.is_none()
                && (account == iss || account_has_ammid(view, account))
            {
                continue;
            }
            return None;
        }
        if step_cur.is_none() && step_iss.is_none() {
            return None;
        }
        if let Some(c) = step_cur {
            cur = c.to_string();
        }
        if let Some(i) = step_iss {
            iss = i.to_string();
        }
        // A `currency: XRP` step is a native boundary: XRP carries no issuer, so
        // clear any issuer inherited from the previous hop and represent the
        // boundary as a drops string (the shape `Leg::parse` decodes as native).
        // Keeping it as an `{currency, issuer}` object would mint a bogus
        // issued-XRP boundary, splitting a single AMM/book hop into two dead hops
        // with no liquidity (the pool/book keylet would never match).
        if cur == "XRP" {
            iss = String::new();
            out.push(serde_json::Value::String("0".into()));
        } else {
            out.push(serde_json::json!({
                "currency": cur,
                "issuer": iss,
                "value": "0",
            }));
        }
    }
    // Append the destination asset (Amount) unless the last step already lands
    // on it.
    let (amt_cur, amt_iss) = boundary_asset(amount);
    if (cur, iss) != (amt_cur, amt_iss) {
        out.push(amount.clone());
    } else {
        // Replace the trailing interior boundary with the real Amount object so
        // the final hop's output template carries Amount's issuer/currency.
        *out.last_mut().unwrap() = amount.clone();
    }
    Some(out)
}

/// Apply a cross-currency Payment that carries `Paths` and a native (XRP)
/// source or destination leg. Resolves the first viable path into a book chain
/// and crosses it forward via the shared byte-exact Taker engine
/// (`cross_path_payment`), spending up to `send_max` to deliver as much of
/// `amount` as the path's liquidity allows.
///
/// The transaction fee is already charged on the account by the engine; this
/// reads that working copy (the source's AccountRoot, which carries any native
/// leg), crosses the chain, enforces the partial-payment gate, then writes the
/// working copy back.
fn apply_paths_payment(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: serde_json::Value,
    send_max: serde_json::Value,
) -> Result<TransactionResult, TransactionResult> {
    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dst_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let src_key = keylet::account(&src_id);
    let bytes = ctx
        .view
        .read(&src_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;

    let paths = ctx
        .tx
        .get("Paths")
        .and_then(|v| v.as_array())
        .ok_or(TransactionResult::TemBadPath)?;

    // Try each alternative path in order; the first that resolves to a pure
    // book chain and crosses is used. (Multi-path blending — rippled's greedy
    // quality-ranked pass — is not yet modelled; a single path covers the
    // validated repro shape.)
    let mut delivered: Option<serde_json::Value> = None;
    for path in paths {
        let Some(path) = path.as_array() else {
            continue;
        };
        let Some(boundaries) = build_path_boundaries(ctx.view, path, &send_max, &amount) else {
            continue;
        };
        match crate::handlers::offer_create::cross_path_payment(
            ctx,
            &src_id,
            &mut acct,
            &dst_id,
            &boundaries,
            &amount,
            &send_max,
        ) {
            Ok((got, _spent)) => {
                delivered = Some(got);
                break;
            }
            Err(_) => continue,
        }
    }
    let Some(delivered) = delivered else {
        return Err(TransactionResult::TecPathPartial);
    };

    // A path that delivers exactly zero is dry (tecPATH_DRY), even under
    // tfPartialPayment — mirrors rippled StrandFlow and lets the build-ledger
    // loop retry once a sibling funds the path.
    if delivered_amount(&delivered) <= 0.0 {
        return Err(TransactionResult::TecPathDry);
    }

    // Partial-payment gate. Without tfPartialPayment the full Amount must be
    // delivered; with it, the delivery must reach DeliverMin (when present),
    // else the requested Amount.
    const TF_PARTIAL_PAYMENT: u32 = rxrpl_protocol::flags::payment::TF_PARTIAL_PAYMENT;
    let partial = helpers::get_flags(ctx.tx) & TF_PARTIAL_PAYMENT != 0;
    if !partial {
        if !delivered_meets_target(&delivered, &amount) {
            return Err(TransactionResult::TecPathPartial);
        }
    } else if let Some((dm_cur, dm_iss, dm_val)) = get_deliver_min_iou(ctx.tx) {
        let (d_cur, d_iss) = asset_of(&delivered);
        let target = serde_json::json!({
            "currency": dm_cur, "issuer": dm_iss, "value": dm_val,
        });
        let _ = (d_cur, d_iss);
        if !delivered_meets_target(&delivered, &target) {
            return Err(TransactionResult::TecPathPartial);
        }
    }

    // rippled Payment::doApply: record sfDeliveredAmount when delivered differs
    // from the requested Amount (partial or path-limited delivery).
    if delivered != amount {
        ctx.view.set_delivered_amount(delivered.clone());
    }

    let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(TransactionResult::TesSuccess)
}

/// Count the transaction's alternative `Paths` that resolve to a pure book/AMM
/// boundary chain of at least two assets. The multi-path Flow gate fires when at
/// least two paths resolve (so a shared AMM is consumed in fib chunks). Unlike
/// [`paths_resolve_to_chain`], this does NOT require an account-ripple step: the
/// multi-path repro's paths are pure currency steps (no `account` field).
fn count_flow_strands(
    view: &dyn ReadView,
    tx: &serde_json::Value,
    send_max: &serde_json::Value,
    amount: &serde_json::Value,
) -> usize {
    let Some(paths) = tx.get("Paths").and_then(|v| v.as_array()) else {
        return 0;
    };
    if paths.len() < 2 {
        return 0;
    }
    paths
        .iter()
        .filter(|p| {
            p.as_array()
                .and_then(|steps| build_path_boundaries(view, steps, send_max, amount))
                .map(|b| b.len() >= 2)
                .unwrap_or(false)
        })
        .count()
}

/// Whether any resolvable strand carries an AMM pool on one of its hops.
///
/// The multi-pass Flow loop (`apply_paths_payment_multi`) exists specifically to
/// consume a *shared AMM pool* in fibonacci-sized chunks across competing
/// strands — the byte-exact behaviour the single-strand/back-solve engines
/// cannot reproduce. A pure-CLOB multi-path payment (no AMM on any hop) is
/// handled correctly by the legacy quality-ranked back-solve
/// (`apply_cross_currency`), which dry-runs every alternative and commits the
/// cheapest. So the Flow gate fires only when an AMM is actually present;
/// routing pure-CLOB multi-path through `flow_multi` (whose forward walk does not
/// reverse-price interior book hops) would mis-size them.
fn flow_strands_have_amm(
    view: &dyn ReadView,
    tx: &serde_json::Value,
    send_max: &serde_json::Value,
    amount: &serde_json::Value,
) -> bool {
    let Some(paths) = tx.get("Paths").and_then(|v| v.as_array()) else {
        return false;
    };
    for p in paths {
        let Some(steps) = p.as_array() else {
            continue;
        };
        let Some(boundaries) = build_path_boundaries(view, steps, send_max, amount) else {
            continue;
        };
        for pair in boundaries.windows(2) {
            let (Some((_, in_cur, in_iss)), Some((_, out_cur, out_iss))) =
                (amm_asset_of(&pair[0]), amm_asset_of(&pair[1]))
            else {
                continue;
            };
            let amm_key = keylet::amm(&in_cur, &in_iss, &out_cur, &out_iss);
            if view.read(&amm_key).is_some() {
                return true;
            }
        }
    }
    false
}

/// A SendMax / Amount value as a `Number` magnitude (XRP drops as an integer,
/// IOU as its decimal value).
fn value_to_number(v: &serde_json::Value) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if let Some(s) = v.as_str() {
        return Number::from_int(s.parse::<i64>().unwrap_or(0));
    }
    match v
        .get("value")
        .and_then(|x| x.as_str())
        .and_then(|s| rxrpl_amount::IOUAmount::from_decimal_string(s).ok())
    {
        Some(iou) => Number::from_iou(&iou),
        None => Number::ZERO,
    }
}

/// Apply a multi-path cross-currency Payment through the multi-pass Flow loop.
/// Builds every resolvable strand, runs `flow_multi` (fib-chunked shared-AMM
/// consumption when two or more strands are live), enforces the partial-payment
/// gate, then writes the source working copy back. Mirrors rippled's `Flow`
/// driving `apply_paths_payment`'s single-strand engine across multiple strands.
fn apply_paths_payment_multi(
    ctx: &mut ApplyContext<'_>,
    account_str: &str,
    destination_str: &str,
    amount: serde_json::Value,
    send_max: serde_json::Value,
) -> Result<TransactionResult, TransactionResult> {
    use rxrpl_amount::number::Number;
    let src_id =
        decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let dst_id =
        decode_account_id(destination_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
    let src_key = keylet::account(&src_id);
    let bytes = ctx
        .view
        .read(&src_key)
        .ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;

    let paths = ctx
        .tx
        .get("Paths")
        .and_then(|v| v.as_array())
        .ok_or(TransactionResult::TemBadPath)?
        .clone();

    // Build every strand that resolves to a boundary chain (>= 2 assets).
    let mut strands = Vec::new();
    for path in &paths {
        let Some(steps) = path.as_array() else {
            continue;
        };
        let Some(boundaries) = build_path_boundaries(ctx.view, steps, &send_max, &amount) else {
            continue;
        };
        if boundaries.len() < 2 {
            continue;
        }
        if let Some(strand) = crate::handlers::offer_create::build_flow_strand(ctx, &boundaries) {
            strands.push(strand);
        }
    }
    if strands.is_empty() {
        return Err(TransactionResult::TecPathDry);
    }

    // Source-funds cap on the SendMax (cannot spend more than held).
    let deliver_req = value_to_number(&amount);
    let send_max_num = value_to_number(&send_max);

    let mut amm_ctx = crate::handlers::flow::AmmContext::new(strands.len() > 1);
    let (delivered_num, _spent_num) = crate::handlers::offer_create::flow_multi(
        ctx,
        &src_id,
        &mut acct,
        &dst_id,
        &strands,
        &deliver_req,
        &send_max_num,
        &mut amm_ctx,
    );

    if delivered_num.is_zero() {
        // Zero delivery is dry, not partial (rippled StrandFlow.h:805).
        return Err(TransactionResult::TecPathDry);
    }

    // The delivered magnitude in the Amount asset, for the partial-payment gate.
    let (amt_cur, amt_iss) = asset_of(&amount);
    let delivered = if amount.is_string() {
        serde_json::Value::String(delivered_num.to_xrp_drops().to_string())
    } else {
        serde_json::json!({
            "currency": amt_cur,
            "issuer": amt_iss,
            "value": delivered_num.to_iou().to_decimal_string(),
        })
    };

    // Partial-payment gate: without tfPartialPayment the full Amount must be
    // delivered; with it, the delivery must reach DeliverMin (when present).
    const TF_PARTIAL_PAYMENT: u32 = rxrpl_protocol::flags::payment::TF_PARTIAL_PAYMENT;
    let partial = helpers::get_flags(ctx.tx) & TF_PARTIAL_PAYMENT != 0;
    if !partial {
        if !delivered_meets_target(&delivered, &amount) {
            return Err(TransactionResult::TecPathPartial);
        }
    } else if let Some((dm_cur, dm_iss, dm_val)) = get_deliver_min_iou(ctx.tx) {
        let target = serde_json::json!({
            "currency": dm_cur, "issuer": dm_iss, "value": dm_val,
        });
        if !delivered_meets_target(&delivered, &target) {
            return Err(TransactionResult::TecPathPartial);
        }
    }
    let _ = Number::ZERO;

    // rippled Payment::doApply: record sfDeliveredAmount when delivered differs
    // from the requested Amount (partial or path-limited delivery).
    if delivered != amount {
        ctx.view.set_delivered_amount(delivered.clone());
    }

    let nb = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(src_key, nb)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(TransactionResult::TesSuccess)
}

/// The asset identity of an Amount value, parsed for AMM lookups: `(is_xrp,
/// currency_bytes, issuer)`. Returns `None` for shapes an AMM can't represent.
fn amm_asset_of(v: &serde_json::Value) -> Option<(bool, [u8; 20], rxrpl_primitives::AccountId)> {
    if v.is_string() {
        return Some((true, [0u8; 20], rxrpl_primitives::AccountId::new([0u8; 20])));
    }
    let cur = v.get("currency")?.as_str()?;
    let iss = v.get("issuer")?.as_str()?;
    let cur_b = helpers::currency_to_bytes(cur);
    let iss_id = decode_account_id(iss).ok()?;
    Some((false, cur_b, iss_id))
}

/// `a <= b` for two like-asset `Number`s.
fn num_le(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    d.is_zero() || d.negative()
}

/// The AMM pool's holding of one asset as a `Number`: the pseudo-account's XRP
/// AccountRoot balance for XRP, or its issuer trust-line balance for an IOU.
fn amm_pool_balance(
    ctx: &ApplyContext<'_>,
    pool: &rxrpl_primitives::AccountId,
    is_xrp: bool,
    cur: &[u8; 20],
    iss: &rxrpl_primitives::AccountId,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    if is_xrp {
        let key = keylet::account(pool);
        let bal = ctx
            .view
            .read(&key)
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|a| helpers::get_balance(&a))
            .unwrap_or(0);
        Number::from_int(bal as i64)
    } else {
        crate::amm_helpers::iou_holding_number(ctx.view, pool, iss, cur)
    }
}

/// Attempt to satisfy a cross-currency conversion through an AMM pool. Returns
/// `Ok(Some(delivered))` with the pool and the user's (= destination, for a
/// conversion) balances mutated byte-exactly, or `Ok(None)` when there is no AMM
/// for the `(send_max, amount)` pair (or it can deliver nothing), leaving the
/// caller to fall back to the order book.
///
/// Mirrors rippled's single-path AMM offer in a `BookStep`: the delivered/spent
/// amounts come from `swapAssetIn`/`swapAssetOut` on the live pool reserves. The
/// trade is output-limited (deliver the full `Amount`) when the required input
/// fits the budget, otherwise input-limited (spend the whole budget — the
/// SendMax capped by the source's funds). The transaction fee was already
/// charged on `user_acct`; XRP legs move through that working copy (the caller
/// writes it back), IOU legs through the trust lines directly.
fn try_amm_conversion(
    ctx: &mut ApplyContext<'_>,
    user_id: &rxrpl_primitives::AccountId,
    user_acct: &mut serde_json::Value,
    amount: &serde_json::Value,
    send_max: &serde_json::Value,
) -> Result<Option<serde_json::Value>, TransactionResult> {
    use rxrpl_amount::number::Number;

    let (out_xrp, out_cur, out_iss) = match amm_asset_of(amount) {
        Some(a) => a,
        None => return Ok(None),
    };
    let (in_xrp, in_cur, in_iss) = match amm_asset_of(send_max) {
        Some(a) => a,
        None => return Ok(None),
    };

    // Locate the AMM SLE for the pair; absent ⇒ no AMM liquidity.
    let amm_key = keylet::amm(&in_cur, &in_iss, &out_cur, &out_iss);
    let Some(amm_bytes) = ctx.view.read(&amm_key) else {
        return Ok(None);
    };
    let Ok(amm): Result<serde_json::Value, _> = serde_json::from_slice(&amm_bytes) else {
        return Ok(None);
    };
    let Some(pool_str) = amm.get("Account").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let Ok(pool_id) = decode_account_id(pool_str) else {
        return Ok(None);
    };
    let tfee = amm.get("TradingFee").and_then(|v| v.as_u64()).unwrap_or(0) as u16;

    // Live pool reserves (rippled `ammAccountHolds`).
    let pool_in = amm_pool_balance(ctx, &pool_id, in_xrp, &in_cur, &in_iss);
    let pool_out = amm_pool_balance(ctx, &pool_id, out_xrp, &out_cur, &out_iss);
    if pool_in.is_zero() || pool_out.is_zero() {
        return Ok(None);
    }

    // Target output (Amount).
    let target_out = if out_xrp {
        Number::from_int(
            amount
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0) as i64,
        )
    } else {
        Number::from_iou(&crate::amm_helpers::parse_iou_value(
            amount.get("value").and_then(|v| v.as_str()).unwrap_or("0"),
        ))
    };

    // Budget input (SendMax), capped at the source's spendable funds.
    let (budget_in, src_in_funds) = if in_xrp {
        let send = send_max
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let avail = helpers::get_balance(user_acct);
        (Number::from_int(send.min(avail) as i64), Number::ZERO)
    } else {
        let send = Number::from_iou(&crate::amm_helpers::parse_iou_value(
            send_max
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("0"),
        ));
        let funds = crate::amm_helpers::iou_holding_number(ctx.view, user_id, &in_iss, &in_cur);
        let budget = if num_le(&send, &funds) { send } else { funds };
        (budget, funds)
    };
    if budget_in.is_zero() || budget_in.negative() {
        return Ok(None);
    }

    // Choose the binding limit. Output-limited when the input needed for the
    // full target fits the budget; otherwise input-limited (spend the budget).
    let in_needed =
        crate::amm_helpers::swap_asset_out(&pool_in, &pool_out, &target_out, tfee, in_xrp);
    let (spent, delivered) = match in_needed {
        Some(needed) if num_le(&needed, &budget_in) => (needed, target_out),
        _ => (
            budget_in,
            crate::amm_helpers::swap_asset_in(&pool_in, &pool_out, &budget_in, tfee, out_xrp),
        ),
    };
    if delivered.is_zero() || spent.is_zero() {
        return Ok(None);
    }

    // --- Apply: move `spent` in from the user to the pool. ---
    if in_xrp {
        let drops = spent.to_xrp_drops();
        let ubal = helpers::get_balance(user_acct);
        helpers::set_balance(user_acct, ubal.saturating_sub(drops));
        let pkey = keylet::account(&pool_id);
        let pbytes = ctx.view.read(&pkey).ok_or(TransactionResult::TecNoEntry)?;
        let mut pacct: serde_json::Value =
            serde_json::from_slice(&pbytes).map_err(|_| TransactionResult::TefInternal)?;
        let pbal = helpers::get_balance(&pacct);
        helpers::set_balance(&mut pacct, pbal + drops);
        let pd = serde_json::to_vec(&pacct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(pkey, pd)
            .map_err(|_| TransactionResult::TefInternal)?;
    } else {
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            user_id,
            &in_iss,
            &in_cur,
            &src_in_funds.sub(&spent),
        )?;
        // rippled's rippleCredit deletes a line drained to zero in the default
        // state (trustDelete); fires inline after the debit.
        crate::handlers::trust_set::maybe_delete_drained_trust_line(
            ctx, user_id, user_acct, &in_iss, &in_cur,
        )?;
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pool_id,
            &in_iss,
            &in_cur,
            &pool_in.add(&spent),
        )?;
    }

    // --- Apply: move `delivered` out from the pool to the user. ---
    if out_xrp {
        let drops = delivered.to_xrp_drops();
        let pkey = keylet::account(&pool_id);
        let pbytes = ctx.view.read(&pkey).ok_or(TransactionResult::TecNoEntry)?;
        let mut pacct: serde_json::Value =
            serde_json::from_slice(&pbytes).map_err(|_| TransactionResult::TefInternal)?;
        let pbal = helpers::get_balance(&pacct);
        helpers::set_balance(&mut pacct, pbal.saturating_sub(drops));
        let pd = serde_json::to_vec(&pacct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(pkey, pd)
            .map_err(|_| TransactionResult::TefInternal)?;
        let ubal = helpers::get_balance(user_acct);
        helpers::set_balance(user_acct, ubal + drops);
    } else {
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            &pool_id,
            &out_iss,
            &out_cur,
            &pool_out.sub(&delivered),
        )?;
        let user_out =
            crate::amm_helpers::iou_holding_number(ctx.view, user_id, &out_iss, &out_cur);
        crate::amm_helpers::set_iou_holding(
            ctx.view,
            user_id,
            &out_iss,
            &out_cur,
            &user_out.add(&delivered),
        )?;
    }

    // Delivered amount in the Amount asset's JSON shape (for the partial gate).
    let delivered_value = if out_xrp {
        serde_json::Value::String(delivered.to_xrp_drops().to_string())
    } else {
        serde_json::json!({
            "currency": amount.get("currency").cloned().unwrap_or_default(),
            "issuer": amount.get("issuer").cloned().unwrap_or_default(),
            "value": delivered.to_iou().to_decimal_string(),
        })
    };
    Ok(Some(delivered_value))
}

/// Whether `delivered` reaches `target` (gate only — f64 comparison is fine for
/// a success/fail decision; the byte-exact amounts come from the Taker engine).
/// Numeric magnitude of a delivered amount, whether it is an XRP drop string or
/// an IOU `{value}` object.
fn delivered_amount(v: &serde_json::Value) -> f64 {
    if let Some(s) = v.as_str() {
        return s.parse().unwrap_or(0.0);
    }
    v.get("value")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

fn delivered_meets_target(delivered: &serde_json::Value, target: &serde_json::Value) -> bool {
    delivered_amount(delivered) + 1e-9 >= delivered_amount(target)
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

    Ok(TransactionResult::TesSuccess)
}

/// Walk a book and collect its offers with parsed IOU amounts, best price
/// (lowest quality) first.
///
/// An XRPL order book is NOT a single paginated directory keyed off the book
/// base: it is a family of quality sub-directories, one per distinct offer
/// rate, each keyed `book_dir_with_quality(book_base, quality)` (the base with
/// its low 64 bits replaced by the rate). They sort by quality, so walking the
/// keyspace via `succ()` from the base — exactly as `cross_offers` /
/// `cross_book_payment` do — visits offers in ascending price. The previous
/// `keylet::dir_node(book_base, page)` pagination read the (usually empty) base
/// index itself, so the book was invisible to the back-solve and the strand
/// never crossed.
fn collect_book_offers(
    ctx: &mut ApplyContext<'_>,
    book_root: &rxrpl_primitives::Hash256,
) -> Vec<CrossOffer> {
    let mut out = Vec::new();
    let book_prefix = book_root.as_bytes()[0..24].to_vec();
    let mut probe = crate::handlers::offer_create::book_dir_with_quality(book_root, 0);
    while let Some(dir_key) = ctx.view.succ(&probe) {
        if dir_key.as_bytes()[0..24] != book_prefix[..] {
            break; // left this book
        }
        probe = dir_key;
        let Some(dir_bytes) = ctx.view.read(&dir_key) else {
            continue;
        };
        let Ok(dir) = serde_json::from_slice::<serde_json::Value>(&dir_bytes) else {
            continue;
        };
        let Some(indexes) = dir.get("Indexes").and_then(|v| v.as_array()) else {
            continue;
        };
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
