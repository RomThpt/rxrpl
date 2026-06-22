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
        // Amount/Amount2 (single-asset withdraw via tfSingleAsset).
        let lp_in = helpers::get_u64_str_field(ctx.tx, "LPTokenIn").unwrap_or(0);
        let amount = ctx
            .tx
            .get("Amount")
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        let amount2 = ctx
            .tx
            .get("Amount2")
            .and_then(amm_helpers::amount_value_drops_or_iou)
            .unwrap_or(0);
        if lp_in == 0 && amount == 0 && amount2 == 0 {
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

        // Withdraw-all flags drain the caller's entire LP position. Without
        // a per-account LP ledger entry we only know about the AMM's
        // Creator; reject any other caller so an account with no LP tokens
        // can't drain the pool via tfWithdrawAll / tfOneAssetWithdrawAll.
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        if flags & (TF_WITHDRAW_ALL | TF_ONE_ASSET_WITHDRAW_ALL) != 0 {
            let creator = amm
                .get("Creator")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if creator != account_str {
                return Err(TransactionResult::TecUnfunded);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amm_key = amm_helpers::compute_amm_key_from_tx(ctx.tx)?;
        let mut amm = amm_helpers::read_amm(ctx.view, &amm_key)?;

        // Byte-exact path: single-asset XRP withdraw of an explicit Amount
        // (tfSingleAsset, no Amount2 / LPTokenIn) on the real AMM model.
        let amount_field = ctx.tx.get("Amount");
        let amount_is_xrp = amount_field.map(|v| v.is_string()).unwrap_or(false);
        let has_lp_in = ctx.tx.get("LPTokenIn").is_some();
        if amount_is_xrp && ctx.tx.get("Amount2").is_none() && !has_lp_in {
            let withdraw = amount_field
                .and_then(amm_helpers::amount_value_drops_or_iou)
                .unwrap_or(0);
            return self.single_xrp_withdraw(ctx, &account_id, &amm_key, &mut amm, withdraw);
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
        helpers::increment_sequence(&mut account);

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
        helpers::increment_sequence(&mut account);
        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
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
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
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
    fn withdraw_full_lp() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenIn": "5000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Full withdraw (LPTokenBalance = 0) auto-deletes the AMM SLE
        // (rippled behavior, mirrored).
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        assert!(sandbox.read(&amm_key).is_none());

        // BOB gets credited only the XRP leg (PoolBalance1 = 10M XRP).
        // The USD leg (5M units) goes to the trust line, not AccountRoot.
        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_bytes = sandbox.read(&bob_key).unwrap();
        let bob: serde_json::Value = serde_json::from_slice(&bob_bytes).unwrap();
        assert_eq!(bob["Balance"].as_str().unwrap(), "60000000");
    }

    #[test]
    fn withdraw_partial_lp() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenIn": "2500000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        // payout1 = 2500000 * 10000000 / 5000000 = 5000000
        // payout2 = 2500000 * 5000000 / 5000000 = 2500000
        assert_eq!(amm["PoolBalance1"].as_str().unwrap(), "5000000");
        assert_eq!(amm["PoolBalance2"].as_str().unwrap(), "2500000");
        assert_eq!(amm["LPTokenBalance"].as_str().unwrap(), "2500000");
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

    #[test]
    fn withdraw_increments_sequence() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "LPTokenIn": "1000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        AMMWithdrawTransactor.apply(&mut ctx).unwrap();

        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_bytes = sandbox.read(&bob_key).unwrap();
        let bob: serde_json::Value = serde_json::from_slice(&bob_bytes).unwrap();
        assert_eq!(bob["Sequence"].as_u64().unwrap(), 2);
    }
}
