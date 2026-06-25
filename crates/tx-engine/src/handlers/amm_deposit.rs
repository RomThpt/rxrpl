use rxrpl_codec::address::classic::{decode_account_id, encode_account_id};
use rxrpl_primitives::AccountId;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct AMMDepositTransactor;

impl Transactor for AMMDepositTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        // Two-asset deposit (default) requires both Amount + Amount2; the
        // tfSingleAsset variant requires exactly one. Any present amount must be
        // strictly positive (fractional IOU values included).
        let amount = ctx.tx.get("Amount");
        let amount2 = ctx.tx.get("Amount2");
        let pos1 = amount.map(amm_helpers::amount_is_positive).unwrap_or(false);
        let pos2 = amount2
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        if amount.is_some() && !pos1 {
            return Err(TransactionResult::TemBadAmount);
        }
        if amount2.is_some() && !pos2 {
            return Err(TransactionResult::TemBadAmount);
        }
        if !pos1 && !pos2 {
            return Err(TransactionResult::TemBadAmount);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        amm_helpers::read_amm(ctx.view, &amm_key)?;

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amount_field = ctx.tx.get("Amount");
        let amount2_field = ctx.tx.get("Amount2");
        let deposit1 = amount_field
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        let deposit2 = amount2_field
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        let pos1 = amount_field
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        let pos2 = amount2_field
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        if !pos1 && !pos2 {
            return Err(TransactionResult::TemBadAmount);
        }

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // Single-asset XRP deposit on the real (rippled) AMM model: the pool
        // balance is the AMM account's own XRP balance, LP tokens are minted by
        // the Number-precise `lpTokensOut`, and the depositor is credited on the
        // real LPToken trust line. Single XRP leg only for now (Amount = XRP,
        // no Amount2).
        let amount_is_xrp = amount_field.map(|v| v.is_string()).unwrap_or(false);
        if amount_is_xrp && amount2_field.is_none() {
            return self.single_xrp_deposit(ctx, &account_id, &amm_key, &mut amm, deposit1);
        }
        // Single-asset IOU deposit: the pool balance is the AMM account's IOU
        // trust-line holding; the deposited IOU moves from the depositor's to
        // the AMM's trust line.
        if let Some(amount) = amount_field {
            if amount.is_object() && amount2_field.is_none() {
                return self.single_iou_deposit(
                    ctx,
                    &account_id,
                    &amm_key,
                    &mut amm,
                    amount.clone(),
                );
            }
        }

        // Two-asset (proportional) deposit on the real AMM model: both Amount
        // and Amount2 present, each an XRP or IOU leg.
        if let (Some(a1), Some(a2)) = (amount_field, amount2_field) {
            let (a1, a2) = (a1.clone(), a2.clone());
            return self.two_asset_deposit(ctx, &account_id, &amm_key, &mut amm, &a1, &a2);
        }

        // Fallback (unreachable for valid deposits): legacy approximate model.
        let pool1 = amm_helpers::get_pool_field(&amm, "PoolBalance1");
        let pool2 = amm_helpers::get_pool_field(&amm, "PoolBalance2");
        let total_lp = amm_helpers::get_pool_field(&amm, "LPTokenBalance");
        let new_lp = if deposit1 > 0 && deposit2 > 0 {
            amm_helpers::compute_lp_tokens_deposit(pool1, pool2, deposit1, deposit2, total_lp)
        } else if deposit1 > 0 {
            amm_helpers::compute_lp_tokens_deposit(pool1, pool2, deposit1, 0, total_lp)
        } else {
            amm_helpers::compute_lp_tokens_deposit(pool2, pool1, deposit2, 0, total_lp)
        };
        amm["PoolBalance1"] = serde_json::Value::String((pool1 + deposit1).to_string());
        amm["PoolBalance2"] = serde_json::Value::String((pool2 + deposit2).to_string());
        amm["LPTokenBalance"] = serde_json::Value::String((total_lp + new_lp).to_string());
        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;
        if new_lp > 0 {
            amm_helpers::adjust_lp_balance(ctx.view, &amm_key, &account_id, new_lp as i128)?;
        }
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let xrp_deducted = xrp_drops_from_amount_opt(amount_field)
            .saturating_add(xrp_drops_from_amount_opt(amount2_field));
        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance.saturating_sub(xrp_deducted));
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

impl AMMDepositTransactor {
    /// Single-asset XRP deposit on the real AMM model (byte-exact path).
    fn single_xrp_deposit(
        &self,
        ctx: &mut ApplyContext<'_>,
        depositor: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        deposit: u64,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let amm_account_str = amm["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let amm_account =
            decode_account_id(&amm_account_str).map_err(|_| TransactionResult::TefInternal)?;
        let tfee = amm["TradingFee"].as_u64().unwrap_or(0) as u16;
        let lpt = amm
            .get("LPTokenBalance")
            .ok_or(TransactionResult::TefInternal)?;
        let lp_currency_hex = lpt["currency"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let total_lp = amm_helpers::parse_iou_value(lpt["value"].as_str().unwrap_or("0"));

        // Pool XRP balance = the AMM account's own balance.
        let amm_acct_key = keylet::account(&amm_account);
        let amm_acct_bytes = ctx
            .view
            .read(&amm_acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut amm_acct: serde_json::Value =
            serde_json::from_slice(&amm_acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pool = helpers::get_balance(&amm_acct);

        let tokens = amm_helpers::lp_tokens_out_single(pool, deposit, &total_lp, tfee);

        // AMM.LPTokenBalance += tokens.
        let new_total = Number::from_iou(&total_lp)
            .add(&Number::from_iou(&tokens))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // AMM account XRP += deposit.
        helpers::set_balance(&mut amm_acct, pool + deposit);
        let amm_acct_data =
            serde_json::to_vec(&amm_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_acct_key, amm_acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit the depositor's LPToken trust line.
        credit_lp_line(ctx, depositor, &amm_account, &lp_currency_hex, &tokens)?;

        // Depositor XRP -= deposit, bump sequence (fee charged by the engine).
        let acct_key = keylet::account(depositor);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal.saturating_sub(deposit));
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }

    /// Single-asset IOU deposit on the real AMM model. The pool balance is the
    /// AMM account's IOU trust-line holding; the deposited IOU moves from the
    /// depositor's trust line to the AMM's.
    fn single_iou_deposit(
        &self,
        ctx: &mut ApplyContext<'_>,
        depositor: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        amount: serde_json::Value,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let amm_account = decode_account_id(
            amm["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let tfee = amm["TradingFee"].as_u64().unwrap_or(0) as u16;
        let lpt = amm
            .get("LPTokenBalance")
            .ok_or(TransactionResult::TefInternal)?;
        let lp_currency_hex = lpt["currency"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let total_lp = amm_helpers::parse_iou_value(lpt["value"].as_str().unwrap_or("0"));

        // Deposited asset (currency + issuer) and amount.
        let asset_issuer = decode_account_id(
            amount["issuer"]
                .as_str()
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let asset_currency =
            helpers::currency_to_bytes(amount["currency"].as_str().unwrap_or_default());
        let deposit = Number::from_iou(&amm_helpers::parse_iou_value(
            amount["value"].as_str().unwrap_or("0"),
        ));

        // Pool = AMM's holding of the deposited asset.
        let pool =
            amm_helpers::iou_holding_number(ctx.view, &amm_account, &asset_issuer, &asset_currency);

        // LP tokens from the deposit/pool ratio, then adjustAssetInByTokens:
        // re-derive tokens and the actual deposit so the rounding can't credit
        // more than the deposit authorises.
        let gt = |a: &Number, b: &Number| {
            let d = a.sub(b);
            !d.negative() && !d.is_zero()
        };
        // STAmount-style subtraction: round the difference onto the IOU grid
        // (ToNearest), matching rippled's STAmount operator-.
        let iou_sub = |a: &Number, b: &Number| Number::from_iou(&a.sub(b).to_iou());
        let r = deposit.div(&pool);
        let tokens0 = amm_helpers::lp_tokens_out_ratio(&r, &total_lp, tfee);
        // adjustAssetInByTokens: re-derive the deposit (assetAdj, rounded up to
        // the IOU grid) consistent with the issued tokens. singleDeposit always
        // applies min(amount, assetAdj), even when no re-rounding is needed.
        let asset_adj0 = amm_helpers::amm_asset_in(&pool, &total_lp, &tokens0, tfee);
        let (tokens, asset_adj) = if gt(&asset_adj0, &deposit) {
            let adj_amount = iou_sub(&deposit, &iou_sub(&asset_adj0, &deposit));
            let r2 = adj_amount.div(&pool);
            let t = amm_helpers::lp_tokens_out_ratio(&r2, &total_lp, tfee);
            let asset_adj1 = amm_helpers::amm_asset_in(&pool, &total_lp, &t, tfee);
            (t, asset_adj1)
        } else {
            (tokens0, asset_adj0)
        };
        let deposit = if gt(&deposit, &asset_adj) {
            asset_adj
        } else {
            deposit
        };

        // AMM.LPTokenBalance += tokens.
        let new_total = Number::from_iou(&total_lp)
            .add(&Number::from_iou(&tokens))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move the deposited IOU: depositor -= deposit, AMM += deposit.
        let dep_hold =
            amm_helpers::iou_holding_number(ctx.view, depositor, &asset_issuer, &asset_currency);
        amm_helpers::set_iou_holding(
            ctx.view,
            depositor,
            &asset_issuer,
            &asset_currency,
            &dep_hold.sub(&deposit),
        )?;
        amm_helpers::set_iou_holding(
            ctx.view,
            &amm_account,
            &asset_issuer,
            &asset_currency,
            &pool.add(&deposit),
        )?;

        // Credit the depositor's LPToken trust line.
        credit_lp_line(ctx, depositor, &amm_account, &lp_currency_hex, &tokens)?;

        // Bump the depositor's sequence (fee charged by the engine).
        let acct_key = keylet::account(depositor);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }

    /// Two-asset proportional deposit (`equalDepositLimit`): both Amount and
    /// Amount2 present, each an XRP or IOU leg. Deposits asset1 in full and the
    /// proportional asset2; if asset2 is the binding limit, deposits asset2 in
    /// full and the proportional asset1.
    fn two_asset_deposit(
        &self,
        ctx: &mut ApplyContext<'_>,
        depositor: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        amount1_field: &serde_json::Value,
        amount2_field: &serde_json::Value,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let amm_account = decode_account_id(
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

        let leg1 = DepositLeg::parse(amount1_field)?;
        let leg2 = DepositLeg::parse(amount2_field)?;
        let amount1 = leg_requested_number(amount1_field, &leg1);
        let amount2 = leg_requested_number(amount2_field, &leg2);
        let pool1 = leg_pool_number(ctx, &amm_account, &leg1);
        let pool2 = leg_pool_number(ctx, &amm_account, &leg2);

        let gt = |a: &Number, b: &Number| {
            let d = a.sub(b);
            !d.negative() && !d.is_zero()
        };
        let frac_by_tokens = |tokens: &rxrpl_amount::IOUAmount| {
            Number::from_iou(tokens).div(&Number::from_iou(&total_lp))
        };

        // asset1 in full, asset2 proportional.
        let frac = amount1.div(&pool1);
        let tokens = amm_helpers::rounded_lp_tokens_deposit(&total_lp, &frac);
        let amount2_dep = leg_rounded_asset(&leg2, &pool2, &frac_by_tokens(&tokens));
        let (dep1, dep2, tokens) = if !gt(&amount2_dep, &amount2) {
            (amount1, amount2_dep, tokens)
        } else {
            // asset2 is the binding limit: asset2 in full, asset1 proportional.
            let frac = amount2.div(&pool2);
            let tokens = amm_helpers::rounded_lp_tokens_deposit(&total_lp, &frac);
            let amount1_dep = leg_rounded_asset(&leg1, &pool1, &frac_by_tokens(&tokens));
            if gt(&amount1_dep, &amount1) {
                return Ok(TransactionResult::TecAmmFailed);
            }
            (amount1_dep, amount2, tokens)
        };

        // AMM.LPTokenBalance += tokens.
        let new_total = Number::from_iou(&total_lp)
            .add(&Number::from_iou(&tokens))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Move each leg into the pool; accumulate the XRP leg to deduct once.
        let mut xrp_deposit = 0u64;
        apply_leg(
            ctx,
            depositor,
            &amm_account,
            &leg1,
            &pool1,
            &dep1,
            &mut xrp_deposit,
        )?;
        apply_leg(
            ctx,
            depositor,
            &amm_account,
            &leg2,
            &pool2,
            &dep2,
            &mut xrp_deposit,
        )?;

        // Credit the depositor's LPToken trust line.
        credit_lp_line(ctx, depositor, &amm_account, &lp_currency_hex, &tokens)?;

        // Depositor XRP -= xrp_deposit, bump sequence (fee charged by the engine).
        let acct_key = keylet::account(depositor);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal.saturating_sub(xrp_deposit));
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// One leg of an AMM deposit: an XRP balance or an IOU trust-line holding.
enum DepositLeg {
    Xrp,
    Iou {
        issuer: rxrpl_primitives::AccountId,
        currency: [u8; 20],
    },
}

impl DepositLeg {
    fn parse(field: &serde_json::Value) -> Result<Self, TransactionResult> {
        if field.is_string() {
            return Ok(DepositLeg::Xrp);
        }
        let issuer = decode_account_id(
            field["issuer"]
                .as_str()
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let currency = helpers::currency_to_bytes(field["currency"].as_str().unwrap_or_default());
        Ok(DepositLeg::Iou { issuer, currency })
    }
}

/// The requested deposit amount of a leg as a `Number` (drops for XRP, IOU
/// value otherwise).
fn leg_requested_number(
    field: &serde_json::Value,
    leg: &DepositLeg,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    match leg {
        DepositLeg::Xrp => Number::from_int(
            field
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0) as i64,
        ),
        DepositLeg::Iou { .. } => Number::from_iou(&amm_helpers::parse_iou_value(
            field["value"].as_str().unwrap_or("0"),
        )),
    }
}

/// The AMM's pool balance for a leg as a `Number`.
fn leg_pool_number(
    ctx: &ApplyContext<'_>,
    amm_account: &rxrpl_primitives::AccountId,
    leg: &DepositLeg,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    match leg {
        DepositLeg::Xrp => {
            let key = keylet::account(amm_account);
            let bal = ctx
                .view
                .read(&key)
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .map(|a| helpers::get_balance(&a))
                .unwrap_or(0);
            Number::from_int(bal as i64)
        }
        DepositLeg::Iou { issuer, currency } => {
            amm_helpers::iou_holding_number(ctx.view, amm_account, issuer, currency)
        }
    }
}

/// `getRoundedAsset(pool, frac, Deposit)` (Upward) for a leg, as a `Number` on
/// the leg's grid (integer drops for XRP, IOU value otherwise).
fn leg_rounded_asset(
    leg: &DepositLeg,
    pool: &rxrpl_amount::number::Number,
    frac: &rxrpl_amount::number::Number,
) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    match leg {
        DepositLeg::Xrp => Number::from_int(amm_helpers::rounded_asset_up_xrp(pool, frac) as i64),
        DepositLeg::Iou { .. } => Number::from_iou(&amm_helpers::rounded_asset_up_iou(pool, frac)),
    }
}

/// Move `dep` of a leg from the depositor into the AMM pool. XRP legs add to the
/// AMM account balance and accumulate `xrp_deposit` to deduct from the depositor
/// once; IOU legs update both trust lines.
fn apply_leg(
    ctx: &mut ApplyContext<'_>,
    depositor: &rxrpl_primitives::AccountId,
    amm_account: &rxrpl_primitives::AccountId,
    leg: &DepositLeg,
    pool: &rxrpl_amount::number::Number,
    dep: &rxrpl_amount::number::Number,
    xrp_deposit: &mut u64,
) -> Result<(), TransactionResult> {
    match leg {
        DepositLeg::Xrp => {
            let drops = dep.to_xrp_drops();
            *xrp_deposit = xrp_deposit.saturating_add(drops);
            let key = keylet::account(amm_account);
            let bytes = ctx.view.read(&key).ok_or(TransactionResult::TerNoAccount)?;
            let mut acct: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
            let bal = helpers::get_balance(&acct);
            helpers::set_balance(&mut acct, bal.saturating_add(drops));
            let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }
        DepositLeg::Iou { issuer, currency } => {
            let dep_hold = amm_helpers::iou_holding_number(ctx.view, depositor, issuer, currency);
            amm_helpers::set_iou_holding(
                ctx.view,
                depositor,
                issuer,
                currency,
                &dep_hold.sub(dep),
            )?;
            amm_helpers::set_iou_holding(ctx.view, amm_account, issuer, currency, &pool.add(dep))?;
        }
    }
    Ok(())
}

/// Add `tokens` LP to the holder's seeded LPToken trust line (`RippleState`).
/// The balance is stored from the low account's perspective.
fn credit_lp_line(
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
    let holder_is_low = holder.as_bytes() < amm_account.as_bytes();
    let tokens_num = Number::from_iou(tokens);

    if let Some(bytes) = ctx.view.read(&tl_key) {
        let mut line: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
        let cur_num = parse_signed_iou(line["Balance"]["value"].as_str().unwrap_or("0"));
        let new_num = if holder_is_low {
            cur_num.add(&tokens_num)
        } else {
            cur_num.sub(&tokens_num)
        };
        line["Balance"]["value"] = serde_json::Value::String(new_num.to_iou().to_decimal_string());
        let data = serde_json::to_vec(&line).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(tl_key, data)
            .map_err(|_| TransactionResult::TefInternal)?;
        return Ok(());
    }

    create_lp_line(
        ctx,
        holder,
        amm_account,
        lp_currency_hex,
        &tokens_num,
        holder_is_low,
        tl_key,
    )
}

/// Create the depositor's LPToken trust line (`RippleState`) on a first deposit
/// to an AMM. Mirrors rippled's `trustCreate` via `accountSend`: the line is
/// linked into both owner directories, carries Reserve + NoRipple on the
/// holder's side, and bumps only the holder's `OwnerCount` (the AMM, as issuer,
/// takes no reserve).
#[allow(clippy::too_many_arguments)]
fn create_lp_line(
    ctx: &mut ApplyContext<'_>,
    holder: &AccountId,
    amm_account: &AccountId,
    lp_currency_hex: &str,
    tokens_num: &rxrpl_amount::number::Number,
    holder_is_low: bool,
    tl_key: rxrpl_primitives::Hash256,
) -> Result<(), TransactionResult> {
    const LSF_LOW_RESERVE: u32 = 0x0001_0000;
    const LSF_HIGH_RESERVE: u32 = 0x0002_0000;
    const LSF_LOW_NO_RIPPLE: u32 = 0x0010_0000;
    const LSF_HIGH_NO_RIPPLE: u32 = 0x0020_0000;

    let balance = if holder_is_low {
        *tokens_num
    } else {
        tokens_num.negate()
    };
    let holder_limit = serde_json::json!({
        "currency": lp_currency_hex,
        "issuer": encode_account_id(holder),
        "value": "0",
    });
    let amm_limit = serde_json::json!({
        "currency": lp_currency_hex,
        "issuer": encode_account_id(amm_account),
        "value": "0",
    });
    let (low_limit, high_limit) = if holder_is_low {
        (holder_limit, amm_limit)
    } else {
        (amm_limit, holder_limit)
    };

    let holder_page = add_to_owner_dir(ctx.view, holder, &tl_key)?;
    let amm_page = add_to_owner_dir(ctx.view, amm_account, &tl_key)?;
    let (low_node, high_node) = if holder_is_low {
        (holder_page, amm_page)
    } else {
        (amm_page, holder_page)
    };

    let flags = if holder_is_low {
        LSF_LOW_RESERVE | LSF_LOW_NO_RIPPLE
    } else {
        LSF_HIGH_RESERVE | LSF_HIGH_NO_RIPPLE
    };

    let mut account_one = [0u8; 20];
    account_one[19] = 1;
    let no_account = encode_account_id(&AccountId::from(account_one));
    let mut tl_obj = serde_json::json!({
        "LedgerEntryType": "RippleState",
        "Balance": { "currency": lp_currency_hex, "issuer": no_account, "value": balance.to_iou().to_decimal_string() },
        "LowLimit": low_limit,
        "HighLimit": high_limit,
        "Flags": flags,
        "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
        "PreviousTxnLgrSeq": 0,
    });
    // LowNode/HighNode are soeDEFAULT(0): rippled omits them when the line lands
    // on page 0 of an owner directory.
    if low_node != 0 {
        tl_obj["LowNode"] = serde_json::Value::String(format!("{low_node:016X}"));
    }
    if high_node != 0 {
        tl_obj["HighNode"] = serde_json::Value::String(format!("{high_node:016X}"));
    }
    let bytes = serde_json::to_vec(&tl_obj).map_err(|_| TransactionResult::TefInternal)?;
    ctx.view
        .insert(tl_key, bytes)
        .map_err(|_| TransactionResult::TefInternal)?;

    let holder_key = keylet::account(holder);
    if let Some(b) = ctx.view.read(&holder_key) {
        let mut acct: serde_json::Value =
            serde_json::from_slice(&b).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut acct, 1);
        let data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(holder_key, data)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}

/// Parse a possibly-signed IOU decimal string into a `Number`.
fn parse_signed_iou(s: &str) -> rxrpl_amount::number::Number {
    use rxrpl_amount::number::Number;
    let neg = s.starts_with('-');
    let mag = s.trim_start_matches('-');
    let iou =
        rxrpl_amount::IOUAmount::from_decimal_string(mag).unwrap_or(rxrpl_amount::IOUAmount::ZERO);
    let n = Number::from_iou(&iou);
    if neg { n.negate() } else { n }
}

fn xrp_drops_from_amount_opt(amount: Option<&serde_json::Value>) -> u64 {
    amount
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const BOB: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    #[test]
    fn reject_zero_deposit() {
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "0",
            "Amount2": "1000000",
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
            AMMDepositTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_missing_asset() {
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "1000000",
            "Amount2": "1000000",
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
            AMMDepositTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_nonexistent_amm() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(BOB).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": BOB,
            "Balance": "50000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "1000000",
            "Amount2": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMDepositTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn accept_single_asset_amount_only() {
        // Per AMMDeposit single-asset variant, only Amount may be present.
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
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
        assert_eq!(AMMDepositTransactor.preflight(&ctx), Ok(()));
    }
}
