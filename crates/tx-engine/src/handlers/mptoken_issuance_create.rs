use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct MPTokenIssuanceCreateTransactor;

impl Transactor for MPTokenIssuanceCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if let Some(transfer_fee) = helpers::get_u32_field(ctx.tx, "TransferFee") {
            if transfer_fee > 50000 {
                return Err(TransactionResult::TemMalformed);
            }
        }

        if let Some(asset_scale) = helpers::get_u32_field(ctx.tx, "AssetScale") {
            if asset_scale > 255 {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Read account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let acct_seq = helpers::get_sequence(&acct);

        // Build issuance entry
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let transfer_fee = helpers::get_u32_field(ctx.tx, "TransferFee").unwrap_or(0);
        let asset_scale = helpers::get_u32_field(ctx.tx, "AssetScale").unwrap_or(0);

        let mut issuance = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": account_str,
            "Sequence": acct_seq,
            "TransferFee": transfer_fee,
            "AssetScale": asset_scale,
            "OutstandingAmount": "0",
            "Flags": tx_flags,
        });

        if let Some(max_amount) = helpers::get_str_field(ctx.tx, "MaximumAmount") {
            issuance["MaximumAmount"] = serde_json::Value::String(max_amount.to_string());
        }

        // Insert issuance
        let issuance_key = keylet::mptoken_issuance(&account_id, acct_seq);
        let issuance_data =
            serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(issuance_key, issuance_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &issuance_key)?;

        // Update account
        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, 1);

        let acct_data = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(acct_key, acct_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
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

    const ISSUER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ISSUER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ISSUER,
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
    fn create_basic_issuance() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify issuance entry exists
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let issuance_bytes = sandbox.read(&issuance_key).unwrap();
        let issuance: serde_json::Value = serde_json::from_slice(&issuance_bytes).unwrap();
        assert_eq!(issuance["Issuer"].as_str().unwrap(), ISSUER);
        assert_eq!(issuance["OutstandingAmount"].as_str().unwrap(), "0");
        assert_eq!(issuance["Sequence"].as_u64().unwrap(), 1);

        // Verify owner count incremented
        let acct_key = keylet::account(&issuer_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn create_issuance_with_optional_fields() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "MaximumAmount": "1000000",
            "TransferFee": 5000,
            "AssetScale": 6,
            "Flags": 0x0022, // lsfMPTCanTransfer | lsfMPTCanLock
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let issuance_bytes = sandbox.read(&issuance_key).unwrap();
        let issuance: serde_json::Value = serde_json::from_slice(&issuance_bytes).unwrap();
        assert_eq!(issuance["MaximumAmount"].as_str().unwrap(), "1000000");
        assert_eq!(issuance["TransferFee"].as_u64().unwrap(), 5000);
        assert_eq!(issuance["AssetScale"].as_u64().unwrap(), 6);
        assert_eq!(issuance["Flags"].as_u64().unwrap(), 0x0022);
    }

    #[test]
    fn reject_transfer_fee_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "TransferFee": 60000,
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
            MPTokenIssuanceCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_asset_scale_too_high() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "AssetScale": 256,
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
            MPTokenIssuanceCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_accepts_valid_params() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "TransferFee": 50000,
            "AssetScale": 255,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(MPTokenIssuanceCreateTransactor.preflight(&ctx), Ok(()));
    }

    #[test]
    fn preclaim_rejects_missing_account() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceCreateTransactor.preclaim(&ctx),
            Err(TransactionResult::TerNoAccount)
        );
    }
}
