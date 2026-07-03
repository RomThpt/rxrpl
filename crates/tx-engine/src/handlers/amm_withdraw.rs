use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct AMMWithdrawTransactor;

/// tfWithdrawAll flag (0x00020000): withdraw all of caller's LP tokens
/// (caller redeems their entire share). Allows zero-amount preflight.
const TF_WITHDRAW_ALL: u32 = 0x00020000;
/// tfOneAssetWithdrawAll flag (0x00040000): redeem all LP for a single
/// asset (caller cashes out into one of the two pool currencies).
const TF_ONE_ASSET_WITHDRAW_ALL: u32 = 0x00040000;
/// tfOneAssetLPToken flag (0x00200000): single-asset payout for a given
/// LPTokenIn (`singleWithdrawTokens`).
const TF_ONE_ASSET_LP_TOKEN: u32 = 0x00200000;

impl Transactor for AMMWithdrawTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(asset)?;
        amm_helpers::validate_asset(asset2)?;

        // tfWithdrawAll / tfOneAssetWithdrawAll: zero amounts allowed —
        // the flag itself signals "withdraw everything".
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        if flags & (TF_WITHDRAW_ALL | TF_ONE_ASSET_WITHDRAW_ALL) != 0 {
            return Ok(());
        }

        // Accept either LPTokenIn (full or partial proportional withdraw) or
        // Amount/Amount2 (single-asset withdraw via tfSingleAsset). LPTokenIn is
        // an LP-currency IOU amount (object).
        let lp_in = ctx
            .tx
            .get("LPTokenIn")
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        let amount = ctx
            .tx
            .get("Amount")
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        let amount2 = ctx
            .tx
            .get("Amount2")
            .map(amm_helpers::amount_is_positive)
            .unwrap_or(false);
        if !lp_in && !amount && !amount2 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        let total_lp = amm_helpers::get_pool_field(&amm, "LPTokenBalance");
        let lp_in = helpers::get_u64_str_field(ctx.tx, "LPTokenIn").unwrap_or(0);
        if lp_in > total_lp {
            return Err(TransactionResult::TecUnfunded);
        }

        // Withdraw-all flags drain the caller's entire LP position: the caller
        // must hold an LPToken trust line with the AMM (real model). The legacy
        // stub had no per-account LP ledger entry and gated on the AMM Creator;
        // here we verify the real trust line exists when the AMM entry carries
        // its LPToken issue.
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        if flags & (TF_WITHDRAW_ALL | TF_ONE_ASSET_WITHDRAW_ALL) != 0 {
            let lpt = amm.get("LPTokenBalance");
            let real = lpt
                .and_then(|l| l.get("currency"))
                .and_then(|v| v.as_str())
                .zip(amm.get("Account").and_then(|v| v.as_str()));
            match real {
                Some((cur_hex, amm_acct_str)) => {
                    let account_id = decode_account_id(account_str)
                        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                    let amm_account = decode_account_id(amm_acct_str)
                        .map_err(|_| TransactionResult::TefInternal)?;
                    let cur_bytes: [u8; 20] = hex::decode(cur_hex)
                        .ok()
                        .and_then(|b| b.try_into().ok())
                        .ok_or(TransactionResult::TefInternal)?;
                    let tl_key = keylet::trust_line(&account_id, &amm_account, &cur_bytes);
                    if !ctx.view.exists(&tl_key) {
                        return Err(TransactionResult::TecUnfunded);
                    }
                }
                None => {
                    // Legacy stub fallback.
                    let creator = amm
                        .get("Creator")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    if creator != account_str {
                        return Err(TransactionResult::TecUnfunded);
                    }
                }
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let _scale = amm_helpers::amm_number_scale_guard(ctx.rules);
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // Byte-exact paths on the real AMM model.
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let amount_field = ctx.tx.get("Amount");
        let amount2_field = ctx.tx.get("Amount2");
        let amount_is_xrp = amount_field.map(|v| v.is_string()).unwrap_or(false);
        let has_lp_in = ctx.tx.get("LPTokenIn").is_some();

        // Single-asset XRP withdraw (tfSingleAsset / tfOneAssetWithdrawAll).
        if amount_is_xrp && amount2_field.is_none() && !has_lp_in {
            if tx_flags & TF_ONE_ASSET_WITHDRAW_ALL != 0 {
                return self.single_xrp_withdraw_all(ctx, &account_id, &amm_key, &mut amm);
            }
            let withdraw = amount_field
                .and_then(amm_helpers::amount_value_drops_or_iou)
                .unwrap_or(0);
            return self.single_xrp_withdraw(ctx, &account_id, &amm_key, &mut amm, withdraw);
        }

        // Single-asset IOU withdraw (tfSingleAsset / tfOneAssetWithdrawAll IOU).
        if let Some(amount) = amount_field {
            if amount.is_object() && amount2_field.is_none() && !has_lp_in {
                let withdraw_all = tx_flags & TF_ONE_ASSET_WITHDRAW_ALL != 0;
                let amount = amount.clone();
                return self.single_iou_withdraw(
                    ctx,
                    &account_id,
                    &amm_key,
                    &mut amm,
                    &amount,
                    withdraw_all,
                );
            }
        }

        // tfLPToken / tfWithdrawAll: proportional withdraw by LP tokens.
        // tfOneAssetLPToken: single-asset payout for a given LPTokenIn.
        if has_lp_in || tx_flags & TF_WITHDRAW_ALL != 0 {
            if tx_flags & TF_ONE_ASSET_LP_TOKEN != 0 {
                let amount = amount_field.cloned().unwrap_or(serde_json::Value::Null);
                return self.single_withdraw_tokens(ctx, &account_id, &amm_key, &mut amm, &amount);
            }
            return self.equal_withdraw_tokens(ctx, &account_id, &amm_key, &mut amm);
        }

        // tfTwoAsset: equal withdraw with per-asset limits.
        if let (Some(a1), Some(a2)) = (amount_field, amount2_field) {
            let (a1, a2) = (a1.clone(), a2.clone());
            return self.equal_withdraw_limit(ctx, &account_id, &amm_key, &mut amm, &a1, &a2);
        }

        let pool1 = amm_helpers::get_pool_field(&amm, "PoolBalance1");
        let pool2 = amm_helpers::get_pool_field(&amm, "PoolBalance2");
        let total_lp = amm_helpers::get_pool_field(&amm, "LPTokenBalance");

        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let withdraw_all = flags & TF_WITHDRAW_ALL != 0;
        // tfWithdrawAll: redeem all pool LP tokens (single-LP scenario for
        // basic AMM with one liquidity provider). For now, set lp_in to total_lp.
        let lp_in = if withdraw_all {
            total_lp
        } else {
            helpers::get_u64_str_field(ctx.tx, "LPTokenIn").unwrap_or(0)
        };
        let withdraw1 = ctx
            .tx
            .get("Amount")
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        let withdraw2 = ctx
            .tx
            .get("Amount2")
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);

        let (payout1, payout2, lp_burned) = if lp_in > 0 {
            let (p1, p2) = amm_helpers::compute_withdraw_amounts(pool1, pool2, lp_in, total_lp);
            (p1, p2, lp_in)
        } else if withdraw1 > 0 || withdraw2 > 0 {
            // Single-asset withdraw: burn LP proportional to the amount taken
            // from its pool. Approximation: lp_burned = amount * total_lp / pool.
            let (payout, pool_for_payout, slot) = if withdraw1 > 0 {
                (withdraw1, pool1, 1)
            } else {
                (withdraw2, pool2, 2)
            };
            if pool_for_payout == 0 {
                return Err(TransactionResult::TecAmmEmpty);
            }
            let lp = ((payout as u128) * (total_lp as u128) / (pool_for_payout as u128)) as u64;
            if slot == 1 {
                (payout.min(pool1), 0, lp)
            } else {
                (0, payout.min(pool2), lp)
            }
        } else {
            return Err(TransactionResult::TemBadAmount);
        };

        let lp_to_burn = lp_burned.min(total_lp);
        let new_lp = total_lp.saturating_sub(lp_to_burn);

        // Debit the holder's LP-token RippleState. Burns LP balance; the
        // line is left in place at zero (rippled keeps the trust line).
        if lp_to_burn > 0 {
            amm_helpers::adjust_lp_balance(ctx.view, &amm_key, &account_id, -(lp_to_burn as i128))?;
        }

        // Auto-delete: when LPTokenBalance hits 0, remove the AMM entirely
        // (rippled behavior — AMMs with no liquidity are garbage-collected).
        if new_lp == 0 {
            ctx.view
                .erase(&amm_key)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            amm["PoolBalance1"] =
                serde_json::Value::String(pool1.saturating_sub(payout1).to_string());
            amm["PoolBalance2"] =
                serde_json::Value::String(pool2.saturating_sub(payout2).to_string());
            amm["LPTokenBalance"] = serde_json::Value::String(new_lp.to_string());

            let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(amm_key, amm_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Credit only XRP payouts to the AccountRoot balance; IOU payouts
        // require trust-line credits (out of scope).
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let asset_field = ctx.tx.get("Asset");
        let asset2_field = ctx.tx.get("Asset2");
        let xrp_payout = if asset_is_xrp(asset_field) {
            payout1
        } else {
            0
        } + if asset_is_xrp(asset2_field) {
            payout2
        } else {
            0
        };
        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance.saturating_add(xrp_payout));

        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

impl AMMWithdrawTransactor {
    /// Single-asset XRP withdraw on the real AMM model (byte-exact path).
    fn single_xrp_withdraw(
        &self,
        ctx: &mut ApplyContext<'_>,
        withdrawer: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        withdraw: u64,
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

        let amm_acct_key = keylet::account(&amm_account);
        let amm_acct_bytes = ctx
            .view
            .read(&amm_acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut amm_acct: serde_json::Value =
            serde_json::from_slice(&amm_acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pool = helpers::get_balance(&amm_acct);

        // LP tokens to burn, and the actual XRP paid out (min of requested and
        // the re-derived ammAssetOut).
        let tokens = amm_helpers::lp_tokens_in_single(pool, withdraw, &total_lp, tfee);
        let asset_out = amm_helpers::amm_asset_out_single_xrp(pool, &total_lp, &tokens, tfee);
        let amount_out = withdraw.min(asset_out);

        // AMM.LPTokenBalance -= tokens.
        let new_total = Number::from_iou(&total_lp)
            .sub(&Number::from_iou(&tokens))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // AMM account XRP -= amount_out.
        helpers::set_balance(&mut amm_acct, pool.saturating_sub(amount_out));
        let amm_acct_data =
            serde_json::to_vec(&amm_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_acct_key, amm_acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Debit the withdrawer's LPToken trust line.
        debit_lp_line(ctx, withdrawer, &amm_account, &lp_currency_hex, &tokens)?;

        // Withdrawer XRP += amount_out, bump sequence.
        let acct_key = keylet::account(withdrawer);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal + amount_out);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }

    /// tfOneAssetWithdrawAll: redeem the holder's entire LP balance into XRP,
    /// deleting their LPToken trust line.
    fn single_xrp_withdraw_all(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
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

        let amm_acct_key = keylet::account(&amm_account);
        let amm_acct_bytes = ctx
            .view
            .read(&amm_acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut amm_acct: serde_json::Value =
            serde_json::from_slice(&amm_acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let pool = helpers::get_balance(&amm_acct);

        // The holder's entire LP balance (magnitude of the trust-line balance).
        let cur_bytes: [u8; 20] = hex::decode(&lp_currency_hex)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(TransactionResult::TefInternal)?;
        let tl_key = keylet::trust_line(holder, &amm_account, &cur_bytes);
        let line_bytes = ctx
            .view
            .read(&tl_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let line: serde_json::Value =
            serde_json::from_slice(&line_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let holder_lp = amm_helpers::parse_iou_value(
            line["Balance"]["value"]
                .as_str()
                .unwrap_or("0")
                .trim_start_matches('-'),
        );

        // XRP paid out for burning the holder's entire LP.
        let amount_out = amm_helpers::amm_asset_out_single_xrp(pool, &total_lp, &holder_lp, tfee);

        // tfOneAssetWithdrawAll: sfAmount is a minimum payout; rippled
        // `singleWithdrawTokens` returns tecAMM_FAILED (no effect) when the full
        // LP position redeems for fewer drops than requested.
        let requested = ctx
            .tx
            .get("Amount")
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        if requested > amount_out {
            return Ok(TransactionResult::TecAmmFailed);
        }

        // rippled `withdraw()` (AMMWithdraw.cpp): a single-asset withdraw whose
        // payout takes the entire XRP side of the pool — `amountWithdrawActual ==
        // curBalance` with `amount2WithdrawActual == none`, or it redeems the
        // pool's whole LP supply (`lpTokensWithdrawActual == lpTokensAMMBalance`)
        // — would strand the other asset, so rippled rejects it with
        // tecAMM_BALANCE (the `curBalance` / `lptBalance` guards). This fires
        // when the withdrawer holds 100% of the LP: `ammAssetOut` with
        // `tokens == totalLP` re-derives `frac == 1`, so `amount_out == pool`.
        // Return before any mutation so only fee + sequence are charged.
        if amount_out >= pool {
            return Ok(TransactionResult::TecAmmBalance);
        }

        // AMM.LPTokenBalance -= holder LP.
        let new_total = Number::from_iou(&total_lp)
            .sub(&Number::from_iou(&holder_lp))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // AMM account XRP -= amount_out.
        helpers::set_balance(&mut amm_acct, pool.saturating_sub(amount_out));
        let amm_acct_data =
            serde_json::to_vec(&amm_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_acct_key, amm_acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Delete the holder's LPToken trust line: unlink from both owner
        // directories (using the line's LowNode/HighNode page hints, since
        // intermediate pages may be unseeded) and erase it. The AMM issuer side
        // carries no reserve.
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
                (holder, low_node, &amm_account, high_node)
            } else {
                (&amm_account, low_node, holder, high_node)
            };
        crate::owner_dir::remove_from_owner_dir_page(ctx.view, low_acct, low_page, &tl_key)?;
        crate::owner_dir::remove_from_owner_dir_page(ctx.view, high_acct, high_page, &tl_key)?;
        ctx.view
            .erase(&tl_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Holder: XRP += amount_out, drop the LP-line reserve, bump sequence.
        let acct_key = keylet::account(holder);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal + amount_out);
        helpers::adjust_owner_count(&mut account, -1);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }

    /// Single-asset IOU withdraw (`singleWithdraw` / `singleWithdrawTokens` when
    /// `withdraw_all`): the pool is the AMM's IOU trust-line holding; the payout
    /// moves from the AMM's trust line to the holder's.
    fn single_iou_withdraw(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        amount: &serde_json::Value,
        withdraw_all: bool,
    ) -> Result<TransactionResult, TransactionResult> {
        let ctx_amm = AmmContext::read(amm)?;
        let leg = WithdrawLeg::parse(amount)?;
        let pool = leg.pool_number(ctx, &ctx_amm.account);

        let tokens = if withdraw_all {
            holder_lp_balance(ctx, holder, &ctx_amm)?
        } else {
            let requested = leg.requested_number(amount);
            let fr = requested.div(&pool);
            let raw = amm_helpers::lp_tokens_in_ratio(&fr, &ctx_amm.total_lp, ctx_amm.tfee);
            amm_helpers::adjust_lp_tokens_withdraw(&ctx_amm.total_lp, &raw)
        };

        let asset_out =
            amm_helpers::amm_asset_out_single_iou(&pool, &ctx_amm.total_lp, &tokens, ctx_amm.tfee);
        // singleWithdraw applies min(amount, assetOut); withdraw_all takes the
        // full re-derived payout.
        let payout = if withdraw_all {
            // tfOneAssetWithdrawAll: rippled `singleWithdrawTokens` treats sfAmount
            // as a minimum and fails with tecAMM_FAILED (no effect) when redeeming
            // the full LP position yields less than it.
            let requested = leg.requested_number(amount);
            if gt(&requested, &asset_out) {
                return Ok(TransactionResult::TecAmmFailed);
            }
            asset_out
        } else {
            let requested = leg.requested_number(amount);
            if gt(&requested, &asset_out) {
                asset_out
            } else {
                requested
            }
        };

        // rippled `withdraw()`: a single-asset withdraw-all that would take the
        // entire IOU side of the pool (sole-LP redemption re-derives `frac == 1`,
        // so `payout == pool`) strands the other asset and is rejected with
        // tecAMM_BALANCE — mirror of the XRP guard in `single_xrp_withdraw_all`.
        if withdraw_all && !gt(&pool, &payout) {
            return Ok(TransactionResult::TecAmmBalance);
        }

        self.burn_and_update_amm(ctx, amm_key, amm, &ctx_amm, &tokens)?;
        leg.pay_out(ctx, holder, &ctx_amm.account, &pool, &payout, &mut 0)?;
        self.finish_withdraw(ctx, holder, &ctx_amm, &tokens, 0)?;
        Ok(TransactionResult::TesSuccess)
    }

    /// `singleWithdrawTokens` (tfOneAssetLPToken): given LPTokenIn, the
    /// single-asset payout is `ammAssetOut(tokens)`.
    fn single_withdraw_tokens(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        amount: &serde_json::Value,
    ) -> Result<TransactionResult, TransactionResult> {
        let ctx_amm = AmmContext::read(amm)?;
        let leg = WithdrawLeg::parse(amount)?;
        let pool = leg.pool_number(ctx, &ctx_amm.account);

        let lp_in = lp_token_in_number(ctx.tx).unwrap_or(rxrpl_amount::IOUAmount::ZERO);
        let tokens = amm_helpers::adjust_lp_tokens_withdraw(&ctx_amm.total_lp, &lp_in);

        let mut xrp_out = 0u64;
        let payout = leg.asset_out(&pool, &ctx_amm, &tokens);

        self.burn_and_update_amm(ctx, amm_key, amm, &ctx_amm, &tokens)?;
        leg.pay_out(ctx, holder, &ctx_amm.account, &pool, &payout, &mut xrp_out)?;
        self.finish_withdraw(ctx, holder, &ctx_amm, &tokens, xrp_out)?;
        Ok(TransactionResult::TesSuccess)
    }

    /// `equalWithdrawTokens` (tfLPToken / tfWithdrawAll): proportional withdraw
    /// of both assets for a given (or full) LPTokenIn.
    fn equal_withdraw_tokens(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let ctx_amm = AmmContext::read(amm)?;
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let withdraw_all = tx_flags & TF_WITHDRAW_ALL != 0;

        let leg1 = WithdrawLeg::parse(&ctx_amm.asset1)?;
        let leg2 = WithdrawLeg::parse(&ctx_amm.asset2)?;
        let pool1 = leg1.pool_number(ctx, &ctx_amm.account);
        let pool2 = leg2.pool_number(ctx, &ctx_amm.account);

        let lp_in = if withdraw_all {
            holder_lp_balance(ctx, holder, &ctx_amm)?
        } else {
            lp_token_in_number(ctx.tx).unwrap_or(rxrpl_amount::IOUAmount::ZERO)
        };

        // Withdrawing all tokens in the pool drains both balances exactly.
        let total = &ctx_amm.total_lp;
        let drains_all = !gt(&Number::from_iou(total), &Number::from_iou(&lp_in))
            && !gt(&Number::from_iou(&lp_in), &Number::from_iou(total));

        let (tokens, payout1, payout2) = if drains_all {
            (lp_in, pool1, pool2)
        } else {
            // rippled `adjustLPTokensIn`: tfWithdrawAll burns the holder's full
            // LP balance unadjusted, so the trust line drains to zero and is
            // deleted; only a partial LPTokenIn withdraw snaps the burned tokens
            // onto the pool's LP grid. Adjusting here too would leave a dust LP
            // balance, so the line (and its owner-directory entry / OwnerCount)
            // would wrongly survive.
            let tokens = if withdraw_all {
                lp_in
            } else {
                amm_helpers::adjust_lp_tokens_withdraw(total, &lp_in)
            };
            // equalWithdrawTokens: frac = divide(tokensAdj, lptAMMBalance,
            // noIssue()) — rounded onto the IOU grid, not a full-precision ratio.
            let frac = Number::from_iou(
                &Number::from_iou(&tokens)
                    .div(&Number::from_iou(total))
                    .to_iou(),
            );
            let p1 = leg1.rounded_asset_down(&pool1, &frac);
            let p2 = leg2.rounded_asset_down(&pool2, &frac);
            (tokens, p1, p2)
        };

        let mut xrp_out = 0u64;
        self.burn_and_update_amm(ctx, amm_key, amm, &ctx_amm, &tokens)?;
        leg1.pay_out(
            ctx,
            holder,
            &ctx_amm.account,
            &pool1,
            &payout1,
            &mut xrp_out,
        )?;
        leg2.pay_out(
            ctx,
            holder,
            &ctx_amm.account,
            &pool2,
            &payout2,
            &mut xrp_out,
        )?;
        self.finish_withdraw(ctx, holder, &ctx_amm, &tokens, xrp_out)?;
        Ok(TransactionResult::TesSuccess)
    }

    /// `equalWithdrawLimit` (tfTwoAsset): equal withdraw with per-asset maxima.
    fn equal_withdraw_limit(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        amount1_field: &serde_json::Value,
        amount2_field: &serde_json::Value,
    ) -> Result<TransactionResult, TransactionResult> {
        use rxrpl_amount::number::Number;

        let ctx_amm = AmmContext::read(amm)?;
        let leg1 = WithdrawLeg::parse(amount1_field)?;
        let leg2 = WithdrawLeg::parse(amount2_field)?;
        let amount1 = leg1.requested_number(amount1_field);
        let amount2 = leg2.requested_number(amount2_field);
        let pool1 = leg1.pool_number(ctx, &ctx_amm.account);
        let pool2 = leg2.pool_number(ctx, &ctx_amm.account);
        let total = &ctx_amm.total_lp;

        // asset1 in full, asset2 proportional.
        let frac = amount1.div(&pool1);
        let tokens = amm_helpers::rounded_lp_tokens_withdraw(total, &frac);
        let frac = Number::from_iou(&tokens).div(&Number::from_iou(total));
        let amount2_w = leg2.rounded_asset_down(&pool2, &frac);
        let (dep1, dep2, tokens) = if !gt(&amount2_w, &amount2) {
            (amount1, amount2_w, tokens)
        } else {
            // asset2 is the binding limit: asset2 in full, asset1 proportional.
            let frac = amount2.div(&pool2);
            let tokens = amm_helpers::rounded_lp_tokens_withdraw(total, &frac);
            let frac = Number::from_iou(&tokens).div(&Number::from_iou(total));
            let amount1_w = leg1.rounded_asset_down(&pool1, &frac);
            if gt(&amount1_w, &amount1) {
                return Ok(TransactionResult::TecAmmFailed);
            }
            (amount1_w, amount2, tokens)
        };

        let mut xrp_out = 0u64;
        self.burn_and_update_amm(ctx, amm_key, amm, &ctx_amm, &tokens)?;
        leg1.pay_out(ctx, holder, &ctx_amm.account, &pool1, &dep1, &mut xrp_out)?;
        leg2.pay_out(ctx, holder, &ctx_amm.account, &pool2, &dep2, &mut xrp_out)?;
        self.finish_withdraw(ctx, holder, &ctx_amm, &tokens, xrp_out)?;
        Ok(TransactionResult::TesSuccess)
    }

    /// AMM.LPTokenBalance -= tokens, write back the AMM entry.
    fn burn_and_update_amm(
        &self,
        ctx: &mut ApplyContext<'_>,
        amm_key: &rxrpl_primitives::Hash256,
        amm: &mut serde_json::Value,
        ctx_amm: &AmmContext,
        tokens: &rxrpl_amount::IOUAmount,
    ) -> Result<(), TransactionResult> {
        use rxrpl_amount::number::Number;
        let new_total = Number::from_iou(&ctx_amm.total_lp)
            .sub(&Number::from_iou(tokens))
            .to_iou();
        amm["LPTokenBalance"]["value"] = serde_json::Value::String(new_total.to_decimal_string());
        let amm_data = serde_json::to_vec(&*amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(*amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;
        Ok(())
    }

    /// Burn the holder's LP, delete the LP line if drained, deduct XRP payouts
    /// already credited to the holder's account, and bump its sequence.
    fn finish_withdraw(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        ctx_amm: &AmmContext,
        tokens: &rxrpl_amount::IOUAmount,
        xrp_out: u64,
    ) -> Result<(), TransactionResult> {
        use rxrpl_amount::number::Number;

        let remaining = {
            let bal = holder_lp_balance(ctx, holder, ctx_amm)?;
            Number::from_iou(&bal)
                .sub(&Number::from_iou(tokens))
                .to_iou()
        };
        let drained = remaining.is_zero();

        if drained {
            delete_lp_line(ctx, holder, &ctx_amm.account, &ctx_amm.lp_currency_hex)?;
        } else {
            debit_lp_line(
                ctx,
                holder,
                &ctx_amm.account,
                &ctx_amm.lp_currency_hex,
                tokens,
            )?;
        }

        let acct_key = keylet::account(holder);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let bal = helpers::get_balance(&account);
        helpers::set_balance(&mut account, bal.saturating_add(xrp_out));
        if drained {
            helpers::adjust_owner_count(&mut account, -1);
        }
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;
        Ok(())
    }
}

/// AMM fields read once for a withdraw variant.
struct AmmContext {
    account: rxrpl_primitives::AccountId,
    asset1: serde_json::Value,
    asset2: serde_json::Value,
    tfee: u16,
    lp_currency_hex: String,
    total_lp: rxrpl_amount::IOUAmount,
}

impl AmmContext {
    fn read(amm: &serde_json::Value) -> Result<Self, TransactionResult> {
        let account = decode_account_id(
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
        Ok(AmmContext {
            account,
            asset1: amm.get("Asset").cloned().unwrap_or(serde_json::Value::Null),
            asset2: amm
                .get("Asset2")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            tfee,
            lp_currency_hex,
            total_lp,
        })
    }
}

/// One leg of an AMM withdraw: a native XRP balance or an IOU trust-line.
enum WithdrawLeg {
    Xrp,
    Iou {
        issuer: rxrpl_primitives::AccountId,
        currency: [u8; 20],
    },
}

impl WithdrawLeg {
    fn parse(field: &serde_json::Value) -> Result<Self, TransactionResult> {
        if field.as_str() == Some("XRP") {
            return Ok(WithdrawLeg::Xrp);
        }
        if field.is_string() {
            // XRP drops string (an Amount field).
            return Ok(WithdrawLeg::Xrp);
        }
        if field["currency"].as_str() == Some("XRP") {
            return Ok(WithdrawLeg::Xrp);
        }
        let issuer = decode_account_id(
            field["issuer"]
                .as_str()
                .ok_or(TransactionResult::TemBadIssuer)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let currency = helpers::currency_to_bytes(field["currency"].as_str().unwrap_or_default());
        Ok(WithdrawLeg::Iou { issuer, currency })
    }

    fn requested_number(&self, field: &serde_json::Value) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            WithdrawLeg::Xrp => Number::from_int(
                field
                    .as_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0) as i64,
            ),
            WithdrawLeg::Iou { .. } => Number::from_iou(&amm_helpers::parse_iou_value(
                field["value"].as_str().unwrap_or("0"),
            )),
        }
    }

    fn pool_number(
        &self,
        ctx: &ApplyContext<'_>,
        amm_account: &rxrpl_primitives::AccountId,
    ) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            WithdrawLeg::Xrp => {
                let key = keylet::account(amm_account);
                let bal = ctx
                    .view
                    .read(&key)
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .map(|a| helpers::get_balance(&a))
                    .unwrap_or(0);
                Number::from_int(bal as i64)
            }
            WithdrawLeg::Iou { issuer, currency } => {
                amm_helpers::iou_holding_number(ctx.view, amm_account, issuer, currency)
            }
        }
    }

    /// `getRoundedAsset(pool, frac, Withdraw)` (Downward) as a `Number` on the
    /// leg's grid.
    fn rounded_asset_down(
        &self,
        pool: &rxrpl_amount::number::Number,
        frac: &rxrpl_amount::number::Number,
    ) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            WithdrawLeg::Xrp => {
                Number::from_int(amm_helpers::rounded_asset_down_xrp(pool, frac) as i64)
            }
            WithdrawLeg::Iou { .. } => {
                Number::from_iou(&amm_helpers::rounded_asset_down_iou(pool, frac))
            }
        }
    }

    /// `ammAssetOut(tokens)` payout for a single-asset leg.
    fn asset_out(
        &self,
        pool: &rxrpl_amount::number::Number,
        ctx_amm: &AmmContext,
        tokens: &rxrpl_amount::IOUAmount,
    ) -> rxrpl_amount::number::Number {
        use rxrpl_amount::number::Number;
        match self {
            WithdrawLeg::Xrp => {
                let drops = amm_helpers::amm_asset_out_single_xrp(
                    pool.to_xrp_drops(),
                    &ctx_amm.total_lp,
                    tokens,
                    ctx_amm.tfee,
                );
                Number::from_int(drops as i64)
            }
            WithdrawLeg::Iou { .. } => {
                amm_helpers::amm_asset_out_single_iou(pool, &ctx_amm.total_lp, tokens, ctx_amm.tfee)
            }
        }
    }

    /// Move `payout` of a leg from the AMM pool to the holder. XRP legs add to
    /// `xrp_out` to credit the holder's account once; IOU legs update both trust
    /// lines.
    fn pay_out(
        &self,
        ctx: &mut ApplyContext<'_>,
        holder: &rxrpl_primitives::AccountId,
        amm_account: &rxrpl_primitives::AccountId,
        pool: &rxrpl_amount::number::Number,
        payout: &rxrpl_amount::number::Number,
        xrp_out: &mut u64,
    ) -> Result<(), TransactionResult> {
        match self {
            WithdrawLeg::Xrp => {
                let drops = payout.to_xrp_drops();
                *xrp_out = xrp_out.saturating_add(drops);
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
            WithdrawLeg::Iou { issuer, currency } => {
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
}

/// The holder's LP balance (magnitude) on its LPToken trust line with the AMM.
fn holder_lp_balance(
    ctx: &ApplyContext<'_>,
    holder: &rxrpl_primitives::AccountId,
    ctx_amm: &AmmContext,
) -> Result<rxrpl_amount::IOUAmount, TransactionResult> {
    let cur_bytes: [u8; 20] = hex::decode(&ctx_amm.lp_currency_hex)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(TransactionResult::TefInternal)?;
    let tl_key = keylet::trust_line(holder, &ctx_amm.account, &cur_bytes);
    let bytes = ctx
        .view
        .read(&tl_key)
        .ok_or(TransactionResult::TecNoEntry)?;
    let line: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;
    Ok(amm_helpers::parse_iou_value(
        line["Balance"]["value"]
            .as_str()
            .unwrap_or("0")
            .trim_start_matches('-'),
    ))
}

/// Parse LPTokenIn (an LP-currency IOU amount) into an `IOUAmount`.
fn lp_token_in_number(tx: &serde_json::Value) -> Option<rxrpl_amount::IOUAmount> {
    let v = tx.get("LPTokenIn")?;
    let s = v.get("value").and_then(|x| x.as_str())?;
    Some(amm_helpers::parse_iou_value(s))
}

/// Compare `a > b` for `Number` operands.
fn gt(a: &rxrpl_amount::number::Number, b: &rxrpl_amount::number::Number) -> bool {
    let d = a.sub(b);
    !d.negative() && !d.is_zero()
}

/// Delete the holder's drained LPToken trust line: unlink from both owner
/// directories using its page hints and erase it (the AMM issuer side has no
/// reserve).
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
    let line_bytes = ctx
        .view
        .read(&tl_key)
        .ok_or(TransactionResult::TecNoEntry)?;
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
    let (low_acct, low_page, high_acct, high_page) = if holder.as_bytes() < amm_account.as_bytes() {
        (holder, low_node, amm_account, high_node)
    } else {
        (amm_account, low_node, holder, high_node)
    };
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, low_acct, low_page, &tl_key)?;
    crate::owner_dir::remove_from_owner_dir_page(ctx.view, high_acct, high_page, &tl_key)?;
    ctx.view
        .erase(&tl_key)
        .map_err(|_| TransactionResult::TefInternal)?;

    // Deleting the line touches the AMM (issuer-side) account's owner directory;
    // rippled re-stamps its PreviousTxnID even though OwnerCount is unchanged
    // (the issuer side took no reserve). Re-write it so the harness stamps it.
    let amm_acct_key = keylet::account(amm_account);
    if let Some(b) = ctx.view.read(&amm_acct_key) {
        ctx.view
            .update(amm_acct_key, b)
            .map_err(|_| TransactionResult::TefInternal)?;
    }
    Ok(())
}

/// Subtract `tokens` LP from the holder's seeded LPToken trust line.
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
    let bytes = ctx
        .view
        .read(&tl_key)
        .ok_or(TransactionResult::TecNoEntry)?;
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
        .map_err(|_| TransactionResult::TefInternal)?;
    Ok(())
}

fn asset_is_xrp(asset: Option<&serde_json::Value>) -> bool {
    let Some(a) = asset else { return false };
    if a.as_str() == Some("XRP") {
        return true;
    }
    a.get("currency").and_then(|c| c.as_str()) == Some("XRP")
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

    fn setup_with_amm(pool1: u64, pool2: u64, lp: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ALICE, 100_000_000u64), (BOB, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

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
            "LPTokenBalance": lp.to_string(),
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
    fn reject_zero_lp_in() {
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenIn": "0",
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
            AMMWithdrawTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_excess_lp_in() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenIn": "9000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMWithdrawTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfunded)
        );
    }

    #[test]
    fn reject_missing_lp_token_in() {
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
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
            AMMWithdrawTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }
}
