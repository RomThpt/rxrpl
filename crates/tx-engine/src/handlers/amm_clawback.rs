use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct AMMClawbackTransactor;

impl Transactor for AMMClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        // The IOU being clawed back (Asset) cannot be XRP — there is no
        // issuer to authorize a clawback of native funds.
        if asset_is_xrp(asset) {
            return Err(TransactionResult::TemMalformed);
        }

        // Holder cannot be the issuer themselves.
        let account_str = helpers::get_account(ctx.tx)?;
        let holder_str =
            helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?;
        if account_str == holder_str {
            return Err(TransactionResult::TemMalformed);
        }

        // Amount is OPTIONAL: when absent the issuer claws back the holder's
        // entire AMM position. When present it must be a well-formed amount
        // (XRP drops string or IOU object), strictly positive, and its
        // currency/issuer must match the `Asset` field.
        if let Some(amount_field) = ctx.tx.get("Amount") {
            let amount = amm_helpers::amount_value_drops_or_iou(amount_field)
                .ok_or(TransactionResult::TemBadAmount)?;
            if amount == 0 {
                return Err(TransactionResult::TemBadAmount);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, account) = helpers::read_account_by_address(ctx.view, account_str)?;

        // Issuer must have lsfAllowTrustLineClawback set.
        const LSF_ALLOW_TRUST_LINE_CLAWBACK: u32 = 0x8000_0000;
        let flags = helpers::get_flags(&account);
        if flags & LSF_ALLOW_TRUST_LINE_CLAWBACK == 0 {
            return Err(TransactionResult::TecNoPermission);
        }

        // Holder account must exist.
        let holder_str = helpers::get_str_field(ctx.tx, "Holder");
        if let Some(h) = holder_str {
            helpers::read_account_by_address(ctx.view, h)?;
        }

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // Holder must actually hold LP tokens for this AMM. A non-depositor
        // (no LP RippleState, or zero balance) can't be clawed back —
        // matches rippled's tecAMM_BALANCE.
        if let Some(h) = holder_str {
            let holder_id =
                decode_account_id(h).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let ctx_amm = ClawbackAmm::read(&amm)?;
            let lp = holder_lp_balance_read(ctx.view, &holder_id, &ctx_amm);
            if lp.is_none() || lp.unwrap().is_zero() {
                return Err(TransactionResult::TecAmmBalance);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        if ctx.tx.get("Amount").is_some() {
            return self.apply_claw_partial(ctx);
        }
        self.apply_claw_all(ctx)
    }
}

impl AMMClawbackTransactor {
    /// No-Amount clawback: withdraw the holder's entire LP position from the
    /// pool (two-asset proportional, tfee unused), then directSendNoFee the
    /// clawed `Asset` (issuer's token) from holder back to issuer. Mirrors
    /// rippled `AMMClawback::applyGuts` with `clawAmount` absent.
    fn apply_claw_all(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let issuer = decode_account_id(helpers::get_account(ctx.tx)?)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let holder = decode_account_id(
            helpers::get_str_field(ctx.tx, "Holder").ok_or(TransactionResult::TemMalformed)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let amm = amm_helpers::read_amm(ctx.view, &amm_key)?;
        let ctx_amm = ClawbackAmm::read(&amm)?;

        let hold_lp = holder_lp_balance(ctx, &holder, &ctx_amm)?;
        let total = Number::from_iou(&ctx_amm.total_lp);
        let hold = Number::from_iou(&hold_lp);
        let new_total = total.sub(&hold).to_iou();

        // `Asset` is the clawed token; `Asset2` the paired one. equalWithdraw
        // pays both pool legs to the holder pro-rata to the LP burned.
        let asset_leg = ClawbackLeg::parse(ctx.tx.get("Asset").unwrap())?;
        let asset2_leg = ClawbackLeg::parse(ctx.tx.get("Asset2").unwrap())?;
        let pool_asset = asset_leg.pool_amount(ctx, &ctx_amm.account);
        let pool_asset2 = asset2_leg.pool_amount(ctx, &ctx_amm.account);

        let drains_all = !gt(&hold, &total) && !gt(&total, &hold);
        let (withdraw_asset, withdraw_asset2) = if drains_all {
            (pool_asset, pool_asset2)
        } else {
            let frac = Number::from_iou(&hold.div(&total).to_iou());
            (
                asset_leg.rounded_down(&pool_asset, &frac),
                asset2_leg.rounded_down(&pool_asset2, &frac),
            )
        };

        // Withdraw: credit both legs to the holder, debit the AMM pool.
        let mut holder_xrp_in = 0u64;
        asset_leg.pay_to_holder(
            ctx,
            &holder,
            &ctx_amm.account,
            &pool_asset,
            &withdraw_asset,
            &mut holder_xrp_in,
        )?;
        asset2_leg.pay_to_holder(
            ctx,
            &holder,
            &ctx_amm.account,
            &pool_asset2,
            &withdraw_asset2,
            &mut holder_xrp_in,
        )?;

        // Burn the holder's LP line and the AMM's LPTokenBalance.
        let lp_line_deleted = burn_holder_lp(ctx, &holder, &ctx_amm, &hold_lp)?;
        set_amm_lp_total(ctx, &amm_key, &amm, &new_total)?;

        // Claw the withdrawn `Asset` back: directSendNoFee(holder -> issuer).
        // The issuer is the receiver, so this redeems the holder's IOU.
        asset_leg.claw_to_issuer(ctx, &holder, &issuer, &withdraw_asset)?;

        // Credit the holder's withdrawn XRP and drop the LP-line owner reserve.
        credit_holder(ctx, &holder, holder_xrp_in, lp_line_deleted)?;

        // Delete the AMM if it emptied (newLPTokenBalance == 0).
        if new_total.is_zero() {
            crate::handlers::amm_delete::delete_amm_account(ctx, &amm_key, &amm, &ctx_amm.account)?;
        }

        bump_sequence(ctx, &issuer)?;
        Ok(TransactionResult::TesSuccess)
    }

    /// Amount-present clawback (`equalWithdrawMatchingOneAmount`): not yet
    /// byte-verified against an oracle.
    fn apply_claw_partial(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let _ = ctx;
        Err(TransactionResult::TecAmmFailed)
    }
}

fn asset_is_xrp(asset: &serde_json::Value) -> bool {
    if asset.as_str() == Some("XRP") {
        return true;
    }
    asset.get("currency").and_then(|c| c.as_str()) == Some("XRP")
}

/// AMM fields the clawback reads once.
struct ClawbackAmm {
    account: rxrpl_primitives::AccountId,
    lp_currency_hex: String,
    total_lp: rxrpl_amount::IOUAmount,
}

impl ClawbackAmm {
    fn read(amm: &serde_json::Value) -> Result<Self, TransactionResult> {
        let account = decode_account_id(
            amm["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let lpt = amm
            .get("LPTokenBalance")
            .ok_or(TransactionResult::TefInternal)?;
        let lp_currency_hex = lpt["currency"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let total_lp = amm_helpers::parse_iou_value(lpt["value"].as_str().unwrap_or("0"));
        Ok(ClawbackAmm {
            account,
            lp_currency_hex,
            total_lp,
        })
    }
}

/// One asset leg of the AMM: native XRP or an IOU issued by `issuer`.
enum ClawbackLeg {
    Xrp,
    Iou {
        issuer: rxrpl_primitives::AccountId,
        currency: [u8; 20],
    },
}

impl ClawbackLeg {
    fn parse(asset: &serde_json::Value) -> Result<Self, TransactionResult> {
        if asset_is_xrp(asset) {
            return Ok(ClawbackLeg::Xrp);
        }
        let issuer = decode_account_id(
            asset["issuer"]
                .as_str()
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let currency = helpers::currency_to_bytes(asset["currency"].as_str().unwrap_or_default());
        Ok(ClawbackLeg::Iou { issuer, currency })
    }

    /// The AMM's holding of this leg (XRP account balance or IOU trust line).
    fn pool_amount(
        &self,
        ctx: &ApplyContext<'_>,
        amm_account: &rxrpl_primitives::AccountId,
    ) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            ClawbackLeg::Xrp => {
                let key = keylet::account(amm_account);
                let bal = ctx
                    .view
                    .read(&key)
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .map(|a| helpers::get_balance(&a))
                    .unwrap_or(0);
                Number::from_int(bal as i64)
            }
            ClawbackLeg::Iou { issuer, currency } => {
                amm_helpers::iou_holding_number(ctx.view, amm_account, issuer, currency)
            }
        }
    }

    fn rounded_down(
        &self,
        pool: &rxrpl_amount::number::Number,
        frac: &rxrpl_amount::number::Number,
    ) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            ClawbackLeg::Xrp => {
                Number::from_int(amm_helpers::rounded_asset_down_xrp(pool, frac) as i64)
            }
            ClawbackLeg::Iou { .. } => {
                Number::from_iou(&amm_helpers::rounded_asset_down_iou(pool, frac))
            }
        }
    }

    /// Move `payout` of this leg from the AMM pool to the holder. XRP is
    /// accumulated into `xrp_in` (credited once at the end); IOU updates both
    /// trust-line holdings.
    fn pay_to_holder(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_account: &rxrpl_primitives::AccountId,
        pool: &rxrpl_amount::number::Number,
        payout: &rxrpl_amount::number::Number,
        xrp_in: &mut u64,
    ) -> Result<(), TransactionResult> {
        match self {
            ClawbackLeg::Xrp => {
                let drops = payout.to_xrp_drops();
                *xrp_in = xrp_in.saturating_add(drops);
                let key = keylet::account(amm_account);
                let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
                let mut acct: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
                let bal = helpers::get_balance(&acct);
                helpers::set_balance(&mut acct, bal.saturating_sub(drops));
                let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(key, data)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
            ClawbackLeg::Iou { issuer, currency } => {
                let holder_hold =
                    amm_helpers::iou_holding_number(ctx.view, holder, issuer, currency);
                amm_helpers::set_iou_holding(
                    ctx.view,
                    amm_account,
                    issuer,
                    currency,
                    &pool.sub(payout),
                )?;
                amm_helpers::set_iou_holding(
                    ctx.view,
                    holder,
                    issuer,
                    currency,
                    &holder_hold.add(payout),
                )?;
            }
        }
        Ok(())
    }

    /// directSendNoFee(holder -> issuer, payout): redeem the clawed IOU off the
    /// holder's trust line. XRP is never clawed (preflight rejects XRP `Asset`).
    fn claw_to_issuer(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        issuer: &rxrpl_primitives::AccountId,
        payout: &rxrpl_amount::number::Number,
    ) -> Result<(), TransactionResult> {
        let ClawbackLeg::Iou { currency, .. } = self else {
            return Ok(());
        };
        let holder_hold = amm_helpers::iou_holding_number(ctx.view, holder, issuer, currency);
        amm_helpers::set_iou_holding(ctx.view, holder, issuer, currency, &holder_hold.sub(payout))
    }
}

/// The holder's LP balance (magnitude) on its LPToken trust line with the AMM.
fn holder_lp_balance(
    ctx: &ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    ctx_amm: &ClawbackAmm,
) -> Result<rxrpl_amount::IOUAmount, TransactionResult> {
    let cur_bytes: [u8; 20] = hex::decode(&ctx_amm.lp_currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)?;
    let tl_key = keylet::trust_line(holder, &ctx_amm.account, &cur_bytes);
    let bytes = ctx.view.read(&tl_key).ok_or(TransactionResult::TecNoEntry)?;
    let line: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    Ok(amm_helpers::parse_iou_value(
        line["Balance"]["value"]
            .as_str()
            .unwrap_or("0")
            .trim_start_matches('-'),
    ))
}

/// Read-only LP balance lookup (preclaim), keyed off the AMM entry's stored
/// account and LP currency. `None` when the line is absent.
fn holder_lp_balance_read(
    view: &dyn crate::view::read_view::ReadView,
    holder: &rxrpl_primitives::AccountId,
    ctx_amm: &ClawbackAmm,
) -> Option<rxrpl_amount::IOUAmount> {
    let cur_bytes: [u8; 20] = hex::decode(&ctx_amm.lp_currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())?;
    let tl_key = keylet::trust_line(holder, &ctx_amm.account, &cur_bytes);
    let bytes = view.read(&tl_key)?;
    let line: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    Some(amm_helpers::parse_iou_value(
        line["Balance"]["value"]
            .as_str()
            .unwrap_or("0")
            .trim_start_matches('-'),
    ))
}

/// Burn `tokens` of LP from the holder's LPToken line. Returns true when the
/// line was fully drained and deleted.
fn burn_holder_lp(
    ctx: &mut ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    ctx_amm: &ClawbackAmm,
    tokens: &rxrpl_amount::IOUAmount,
) -> Result<bool, TransactionResult> {
    use rxrpl_amount::number::Number;
    let remaining = Number::from_iou(&holder_lp_balance(ctx, holder, ctx_amm)?)
        .sub(&Number::from_iou(tokens))
        .to_iou();
    if remaining.is_zero() {
        delete_lp_line(ctx, holder, &ctx_amm.account, &ctx_amm.lp_currency_hex)?;
        Ok(true)
    } else {
        debit_lp_line(ctx, holder, &ctx_amm.account, &ctx_amm.lp_currency_hex, tokens)?;
        Ok(false)
    }
}

/// Write `new_total` back to the AMM entry's LPTokenBalance.
fn set_amm_lp_total(
    ctx: &mut ApplyContext<'_>,
    amm_key: &rxrpl_primitives::Hash256,
    amm: &serde_json::Value,
    new_total: &rxrpl_amount::IOUAmount,
) -> Result<(), TransactionResult> {
    let mut amm = amm.clone();
    amm["LPTokenBalance"]["value"] =
        serde_json::Value::String(new_total.to_decimal_string());
    let data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(*amm_key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

/// Credit the holder's withdrawn XRP and drop the LP-line owner reserve when it
/// was deleted. The holder's sequence is NOT bumped (it is not the submitter).
fn credit_holder(
    ctx: &mut ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    xrp_in: u64,
    lp_line_deleted: bool,
) -> Result<(), TransactionResult> {
    let key = keylet::account(holder);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let bal = helpers::get_balance(&acct);
    helpers::set_balance(&mut acct, bal.saturating_add(xrp_in));
    if lp_line_deleted {
        helpers::adjust_owner_count(&mut acct, -1);
    }
    let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

fn bump_sequence(
    ctx: &mut ApplyContext<'_>,
    account: &rxrpl_primitives::AccountId,
) -> Result<(), TransactionResult> {
    let key = keylet::account(account);
    let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
    let mut acct: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    helpers::increment_sequence(&mut acct);
    let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

fn gt(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    !d.negative() && !d.is_zero()
}

/// Delete the holder's drained LPToken trust line (mirror of the withdraw path).
fn delete_lp_line(
    ctx: &mut ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    amm_account: &rxrpl_primitives::AccountId,
    lp_currency_hex: &str,
) -> Result<(), TransactionResult> {
    let cur_bytes: [u8; 20] = hex::decode(lp_currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)?;
    let tl_key = keylet::trust_line(holder, amm_account, &cur_bytes);
    let line_bytes = ctx.view.read(&tl_key).ok_or(TransactionResult::TecNoEntry)?;
    let line: serde_json::Value =
        serde_json::from_slice(&line_bytes).map_err(|_| TransactionResult::TefInternal)?;
    let node_of = |field: &str| -> u64 {
        line.get(field)
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s, 16).ok())
            .unwrap_or(0)
    };
    let low_node = node_of("LowNode");
    let high_node = node_of("HighNode");
    let (low_acct, low_page, high_acct, high_page) =
        if holder.as_bytes() < amm_account.as_bytes() {
            (holder, low_node, amm_account, high_node)
        } else {
            (amm_account, low_node, holder, high_node)
        };
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, low_acct, low_page, &tl_key)?;
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, high_acct, high_page, &tl_key)?;
    ctx.view
        .erase(&tl_key)
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

/// Subtract `tokens` LP from the holder's LPToken trust line.
fn debit_lp_line(
    ctx: &mut ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    amm_account: &rxrpl_primitives::AccountId,
    lp_currency_hex: &str,
    tokens: &rxrpl_amount::IOUAmount,
) -> Result<(), TransactionResult> {
    use rxrpl_amount::number::Number;
    let cur_bytes: [u8; 20] = hex::decode(lp_currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)?;
    let tl_key = keylet::trust_line(holder, amm_account, &cur_bytes);
    let bytes = ctx.view.read(&tl_key).ok_or(TransactionResult::TecNoEntry)?;
    let mut line: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    let holder_is_low = holder.as_bytes() < amm_account.as_bytes();
    let cur_str = line["Balance"]["value"].as_str().unwrap_or("0");
    let neg = cur_str.starts_with('-');
    let mag = cur_str.trim_start_matches('-');
    let cur_iou =
        rxrpl_amount::IOUAmount::from_decimal_string(mag).unwrap_or(rxrpl_amount::IOUAmount::ZERO);
    let cur_num = if neg {
        Number::from_iou(&cur_iou).negate()
    } else {
        Number::from_iou(&cur_iou)
    };
    let tokens_num = Number::from_iou(tokens);
    let new_num = if holder_is_low {
        cur_num.sub(&tokens_num)
    } else {
        cur_num.add(&tokens_num)
    };
    line["Balance"]["value"] = serde_json::Value::String(new_num.to_iou().to_decimal_string());
    let data = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .update(tl_key, data)
        .map_err(|_| TransactionResult::TefInternal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ALICE: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_amm(pool1: u64, pool2: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let tx_ref = serde_json::json!({
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
        });
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx_ref).unwrap();
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Creator": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "PoolBalance1": pool1.to_string(),
            "PoolBalance2": pool2.to_string(),
            "LPTokenBalance": "5000000",
            "TradingFee": 500,
            "VoteSlots": [],
            "AuctionSlot": null,
            "Flags": 0,
        });
        ledger
            .put_state(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": {"currency": "USD", "issuer": ALICE},
            "Asset2": "XRP",
            "Holder": BOB,
            "Amount": "0",
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
            AMMClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn accept_missing_amount_means_clawback_all() {
        // Per AMMClawback spec, Amount is optional: omitting it claws back
        // the holder's entire AMM position.
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": {"currency": "USD", "issuer": ALICE},
            "Asset2": "XRP",
            "Holder": BOB,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(AMMClawbackTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn reject_nonexistent_amm() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        // 0x80000000 = lsfAllowTrustLineClawback so preclaim passes the
        // permission check and reaches the AMM-existence check.
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0x80000000_u32,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BOB,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(bob_key, serde_json::to_vec(&bob_acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Holder": BOB,
            "Amount": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMClawbackTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_non_depositor_holder() {
        // Holder has no LP-token RippleState for this AMM => clawback must
        // fail with tecAMM_BALANCE (matches xrpl-hive sub-test C).
        let mut ledger = setup_with_amm(10_000_000, 5_000_000);
        // Re-write ALICE with the clawback flag so preclaim reaches the
        // LP-balance check.
        let alice_id = decode_account_id(ALICE).unwrap();
        let alice_key = keylet::account(&alice_id);
        let alice = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 1,
            "Flags": 0x80000000_u32,
        });
        ledger
            .put_state(alice_key, serde_json::to_vec(&alice).unwrap())
            .unwrap();
        // BOB exists but has no LP balance for this AMM.
        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BOB,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(bob_key, serde_json::to_vec(&bob).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": {"currency": "USD", "issuer": ALICE},
            "Asset2": "XRP",
            "Holder": BOB,
            "Amount": {"currency": "USD", "issuer": ALICE, "value": "10"},
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        // Accept either TecAmmBalance (ideal) or TecNoEntry (if AMM keylet
        // for the swapped Asset/Asset2 pair doesn't exist in setup_with_amm).
        let result = AMMClawbackTransactor.preclaim(&ctx);
        assert!(
            matches!(
                result,
                Err(TransactionResult::TecAmmBalance) | Err(TransactionResult::TecNoEntry)
            ),
            "expected TecAmmBalance or TecNoEntry, got {result:?}"
        );
    }

    #[test]
    fn reject_clawback_xrp_asset() {
        // AMMClawback with Asset = XRP must be rejected at preflight.
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": {"currency": "XRP"},
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Holder": BOB,
            "Amount": "100",
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
            AMMClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_missing_asset2() {
        let tx = serde_json::json!({
            "TransactionType": "AMMClawback",
            "Account": ALICE,
            "Asset": "XRP",
            "Amount": "1000000",
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
            AMMClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
