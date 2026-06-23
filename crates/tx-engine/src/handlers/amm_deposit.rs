use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
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
        // tfSingleAsset variant requires exactly one of them.
        let amount = ctx
            .tx
            .get("Amount")
            .and_then(amm_helpers::amount_value_drops_or_iou);
        let amount2 = ctx
            .tx
            .get("Amount2")
            .and_then(amm_helpers::amount_value_drops_or_iou);

        match (amount, amount2) {
            (Some(a), Some(b)) if a > 0 && b > 0 => Ok(()),
            (Some(a), None) if a > 0 => Ok(()),
            (None, Some(b)) if b > 0 => Ok(()),
            _ => Err(TransactionResult::TemBadAmount),
        }
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
        if deposit1 == 0 && deposit2 == 0 {
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
                return self.single_iou_deposit(ctx, &account_id, &amm_key, &mut amm, amount.clone());
            }
        }

        // Fallback (two-asset legs): legacy approximate model.
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
    let bytes = ctx
        .view
        .read(&tl_key)
        .ok_or(TransactionResult::TecNoEntry)?;
    let mut line: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TefInternal)?;

    let holder_is_low = holder.as_bytes() < amm_account.as_bytes();
    let cur_str = line["Balance"]["value"].as_str().unwrap_or("0");
    let cur_num = parse_signed_iou(cur_str);
    let tokens_num = Number::from_iou(tokens);
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
    fn deposit_proportional() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "2000000",
            "Amount2": "1000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMDepositTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        assert_eq!(amm["PoolBalance1"].as_str().unwrap(), "12000000");
        assert_eq!(amm["PoolBalance2"].as_str().unwrap(), "6000000");

        // LP tokens: 2000000 * 5000000 / 10000000 = 1000000
        assert_eq!(amm["LPTokenBalance"].as_str().unwrap(), "6000000");
    }

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
    fn deposit_updates_depositor_balance() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "2000000",
            "Amount2": "1000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        AMMDepositTransactor.apply(&mut ctx).unwrap();

        let bob_id = decode_account_id(BOB).unwrap();
        let bob_key = keylet::account(&bob_id);
        let bob_bytes = sandbox.read(&bob_key).unwrap();
        let bob: serde_json::Value = serde_json::from_slice(&bob_bytes).unwrap();
        assert_eq!(bob["Balance"].as_str().unwrap(), "47000000");
        assert_eq!(bob["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn deposit_mints_lp_ripple_state() {
        let ledger = setup_with_amm(10_000_000, 5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMDeposit",
            "Account": BOB,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": BOB},
            "Amount": "2000000",
            "Amount2": "1000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        AMMDepositTransactor.apply(&mut ctx).unwrap();

        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let bob_id = decode_account_id(BOB).unwrap();
        // 2_000_000 * 5_000_000 / 10_000_000 = 1_000_000 LP minted.
        assert_eq!(
            amm_helpers::lp_balance_of(&sandbox, &amm_key, &bob_id),
            1_000_000
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
