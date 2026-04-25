use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::amm_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct AMMCreateTransactor;

impl Transactor for AMMCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // rippled's AMMCreate derives Asset/Asset2 from Amount/Amount2 when
        // they're absent. Mirror that so xrpl-hive's two-arg payload is
        // accepted.
        let amount_field = ctx.tx.get("Amount").ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx
            .tx
            .get("Amount2")
            .ok_or(TransactionResult::TemBadAmount)?;

        let asset = ctx
            .tx
            .get("Asset")
            .cloned()
            .or_else(|| amm_helpers::asset_spec_from_amount(amount_field))
            .ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .cloned()
            .or_else(|| amm_helpers::asset_spec_from_amount(amount2_field))
            .ok_or(TransactionResult::TemMalformed)?;

        amm_helpers::validate_asset(&asset)?;
        amm_helpers::validate_asset(&asset2)?;

        if !amm_helpers::assets_differ(&asset, &asset2) {
            return Err(TransactionResult::TemMalformed);
        }

        let amount = amm_helpers::amount_value_drops_or_iou(amount_field)
            .ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        let amount2 = amm_helpers::amount_value_drops_or_iou(amount2_field)
            .ok_or(TransactionResult::TemBadAmount)?;
        if amount2 == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        if let Some(fee) = helpers::get_u32_field(ctx.tx, "TradingFee") {
            if fee > 1000 {
                return Err(TransactionResult::TemBadFee);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, account) = helpers::read_account_by_address(ctx.view, account_str)?;

        // AMM must not already exist (use derived assets when absent).
        let amm_key = amm_key_from_tx_with_derivation(ctx.tx)?;
        if ctx.view.exists(&amm_key) {
            return Err(TransactionResult::TecDuplicate);
        }

        // Creator must have sufficient XRP balance for the XRP leg(s) only.
        // For IOU legs, trust-line balance enforcement is out of scope here
        // (covered by a future trust-line debit pass).
        let balance = helpers::get_balance(&account);
        let amount_field = ctx.tx.get("Amount").ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx.tx.get("Amount2").ok_or(TransactionResult::TemBadAmount)?;
        let xrp_needed = xrp_drops_from_amount(amount_field)
            .checked_add(xrp_drops_from_amount(amount2_field))
            .ok_or(TransactionResult::TemBadAmount)?;
        if balance < xrp_needed {
            return Err(TransactionResult::TecUnfunded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let amount_field = ctx.tx.get("Amount").ok_or(TransactionResult::TemBadAmount)?;
        let amount2_field = ctx.tx.get("Amount2").ok_or(TransactionResult::TemBadAmount)?;

        let amount = amm_helpers::amount_value_drops_or_iou(amount_field)
            .ok_or(TransactionResult::TemBadAmount)?;
        let amount2 = amm_helpers::amount_value_drops_or_iou(amount2_field)
            .ok_or(TransactionResult::TemBadAmount)?;

        let trading_fee = helpers::get_u32_field(ctx.tx, "TradingFee").unwrap_or(0);

        // Compute LP tokens
        let lp_tokens = amm_helpers::compute_lp_tokens_initial(amount, amount2);

        // Build AMM entry — Asset/Asset2 may have been omitted by the caller;
        // derive from Amount/Amount2 in that case.
        let asset = ctx
            .tx
            .get("Asset")
            .cloned()
            .or_else(|| amm_helpers::asset_spec_from_amount(amount_field))
            .ok_or(TransactionResult::TemMalformed)?;
        let asset2 = ctx
            .tx
            .get("Asset2")
            .cloned()
            .or_else(|| amm_helpers::asset_spec_from_amount(amount2_field))
            .ok_or(TransactionResult::TemMalformed)?;

        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "Creator": account_str,
            "Asset": asset,
            "Asset2": asset2,
            "PoolBalance1": amount.to_string(),
            "PoolBalance2": amount2.to_string(),
            "LPTokenBalance": lp_tokens.to_string(),
            "TradingFee": trading_fee,
            "VoteSlots": [],
            "AuctionSlot": null,
            "Flags": 0,
        });

        let amm_key = amm_key_from_tx_with_derivation(ctx.tx)?;
        let amm_data = serde_json::to_vec(&amm).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(amm_key, amm_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Deduct only XRP legs from the creator's AccountRoot. IOU legs would
        // need a trust-line debit (out of scope until non-issuer IOU sends
        // are implemented in payment.rs).
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let xrp_deducted = xrp_drops_from_amount(amount_field)
            .saturating_add(xrp_drops_from_amount(amount2_field));
        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance.saturating_sub(xrp_deducted));
        helpers::increment_sequence(&mut account);
        helpers::adjust_owner_count(&mut account, 1);

        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

/// Compute the AMM keylet from a tx, deriving Asset/Asset2 from Amount/Amount2
/// when explicit Asset fields are absent.
fn amm_key_from_tx_with_derivation(
    tx: &serde_json::Value,
) -> Result<rxrpl_primitives::Hash256, TransactionResult> {
    let asset = tx
        .get("Asset")
        .cloned()
        .or_else(|| tx.get("Amount").and_then(amm_helpers::asset_spec_from_amount))
        .ok_or(TransactionResult::TemMalformed)?;
    let asset2 = tx
        .get("Asset2")
        .cloned()
        .or_else(|| tx.get("Amount2").and_then(amm_helpers::asset_spec_from_amount))
        .ok_or(TransactionResult::TemMalformed)?;
    amm_helpers::compute_amm_key(&asset, &asset2)
}

/// Return the XRP drops carried by an Amount field, 0 if it's an IOU.
fn xrp_drops_from_amount(amount: &serde_json::Value) -> u64 {
    amount
        .as_str()
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

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ALICE).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ALICE,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn create_xrp_xrp_pool() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
            "Amount": "10000000",
            "Amount2": "5000000",
            "TradingFee": 500,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AMMCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify AMM exists
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm_bytes = sandbox.read(&amm_key).unwrap();
        let amm: serde_json::Value = serde_json::from_slice(&amm_bytes).unwrap();
        assert_eq!(amm["LedgerEntryType"].as_str().unwrap(), "AMM");
        assert_eq!(amm["PoolBalance1"].as_str().unwrap(), "10000000");
        assert_eq!(amm["PoolBalance2"].as_str().unwrap(), "5000000");
        assert_eq!(amm["LPTokenBalance"].as_str().unwrap(), "5000000");
        assert_eq!(amm["TradingFee"].as_u64().unwrap(), 500);

        // Verify balance deducted
        let id = decode_account_id(ALICE).unwrap();
        let acct_key = keylet::account(&id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "85000000");
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn reject_same_assets() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": "XRP",
            "Amount": "10000000",
            "Amount2": "5000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
            "Amount": "0",
            "Amount2": "5000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_trading_fee_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
            "Amount": "10000000",
            "Amount2": "5000000",
            "TradingFee": 1001,
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadFee)
        );
    }

    #[test]
    fn reject_missing_asset2() {
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Amount": "10000000",
            "Amount2": "5000000",
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
            AMMCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_insufficient_balance() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
            "Amount": "80000000",
            "Amount2": "80000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfunded)
        );
    }

    #[test]
    fn reject_duplicate_amm() {
        let mut ledger = setup_accounts();

        // Pre-insert an AMM
        let tx = serde_json::json!({
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
        });
        let amm_key = amm_helpers::compute_amm_key_from_tx(&tx).unwrap();
        let amm = serde_json::json!({
            "LedgerEntryType": "AMM",
            "PoolBalance1": "100",
            "PoolBalance2": "200",
            "LPTokenBalance": "100",
        });
        ledger
            .put_state(amm_key, serde_json::to_vec(&amm).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "AMMCreate",
            "Account": ALICE,
            "Asset": "XRP",
            "Asset2": {"currency": "USD", "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"},
            "Amount": "10000000",
            "Amount2": "5000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AMMCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }
}
