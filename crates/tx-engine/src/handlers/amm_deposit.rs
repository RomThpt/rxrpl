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

        let pool1 = amm_helpers::get_pool_field(&amm, "PoolBalance1");
        let pool2 = amm_helpers::get_pool_field(&amm, "PoolBalance2");
        let total_lp = amm_helpers::get_pool_field(&amm, "LPTokenBalance");

        // For two-asset proportional deposits, mint LP based on the first leg.
        // For single-asset (only one of deposit1/deposit2 is non-zero), mint
        // LP based on the non-zero leg's share of its pool.
        let new_lp = if deposit1 > 0 && deposit2 > 0 {
            amm_helpers::compute_lp_tokens_deposit(pool1, pool2, deposit1, deposit2, total_lp)
        } else if deposit1 > 0 {
            amm_helpers::compute_lp_tokens_deposit(pool1, pool2, deposit1, 0, total_lp)
        } else {
            amm_helpers::compute_lp_tokens_deposit(pool2, pool1, deposit2, 0, total_lp)
        };

        // Update AMM entry
        amm["PoolBalance1"] = serde_json::Value::String((pool1 + deposit1).to_string());
        amm["PoolBalance2"] = serde_json::Value::String((pool2 + deposit2).to_string());
        amm["LPTokenBalance"] = serde_json::Value::String((total_lp + new_lp).to_string());

        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Deduct deposits from account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Only deduct the XRP leg(s) from the AccountRoot balance; IOU
        // legs would require trust-line debits (out of scope).
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
