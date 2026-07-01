use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// The universal transaction-flag bits (`tfFullyCanonicalSig | tfInnerBatchTxn`)
/// that rippled strips before storing sfFlags on the created ledger entry.
const TF_UNIVERSAL: u32 = 0x8000_0000 | 0x4000_0000;

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

        // The issuance keylet/Sequence is the TX seq-proxy value (the engine
        // already consumed the sender's Sequence/Ticket centrally).
        let acct_seq = helpers::tx_seq_proxy_value(ctx.tx);

        // Build issuance entry. rippled stores sfFlags with the universal
        // (tfFullyCanonicalSig | tfInnerBatchTxn) bits stripped; the remaining
        // MPT ledger flags map bit-for-bit onto the tx flags.
        let tx_flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        let stored_flags = tx_flags & !TF_UNIVERSAL;

        // Link the issuance into the issuer's owner directory first so the page
        // number can be recorded as the SoeRequired sfOwnerNode.
        let issuance_key = keylet::mptoken_issuance(&account_id, acct_seq);
        let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &issuance_key)?;

        let mut issuance = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Flags": stored_flags,
            "Issuer": account_str,
            // OutstandingAmount is SoeRequired and rippled serializes it at 0.
            "OutstandingAmount": "0",
            "OwnerNode": format!("{owner_node:016X}"),
            "Sequence": acct_seq,
            // Placeholder filled by the engine's central PreviousTxnID stamping.
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });

        // Optional fields are only serialized when the transaction carries them
        // (mirrors rippled's `if (args.foo)` guards).
        if let Some(max_amount) = helpers::get_str_field(ctx.tx, "MaximumAmount") {
            issuance["MaximumAmount"] = serde_json::Value::String(max_amount.to_string());
        }
        if let Some(asset_scale) = helpers::get_u32_field(ctx.tx, "AssetScale") {
            issuance["AssetScale"] = serde_json::Value::from(asset_scale);
        }
        if let Some(transfer_fee) = helpers::get_u32_field(ctx.tx, "TransferFee") {
            issuance["TransferFee"] = serde_json::Value::from(transfer_fee);
        }
        if let Some(metadata) = helpers::get_str_field(ctx.tx, "MPTokenMetadata") {
            issuance["MPTokenMetadata"] = serde_json::Value::String(metadata.to_string());
        }

        // Insert issuance
        let issuance_data =
            serde_json::to_vec(&issuance).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(issuance_key, issuance_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update account
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

        // Engine consumes the sender's Sequence/Ticket centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
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
        // OutstandingAmount is SoeRequired and rippled serializes it at 0.
        assert_eq!(issuance["OutstandingAmount"].as_str().unwrap(), "0");
        // OwnerNode (the owner-directory page) is SoeRequired and must be set.
        assert_eq!(issuance["OwnerNode"].as_str().unwrap(), "0000000000000000");
        assert_eq!(issuance["Flags"].as_u64().unwrap(), 0);
        // A bare issuance carries no AssetScale / TransferFee (both optional).
        assert!(issuance.get("AssetScale").is_none());
        assert!(issuance.get("TransferFee").is_none());
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
        assert_eq!(issuance["OutstandingAmount"].as_str().unwrap(), "0");
        assert_eq!(issuance["OwnerNode"].as_str().unwrap(), "0000000000000000");
    }

    #[test]
    fn stored_flags_strip_universal_bits() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        // tfFullyCanonicalSig (0x80000000) | tfMPTCanTransfer (0x20).
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceCreate",
            "Account": ISSUER,
            "Flags": 0x8000_0020u32,
            "Fee": "12",
            "Sequence": 1,
        });
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            MPTokenIssuanceCreateTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let issuance: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&issuance_key).unwrap()).unwrap();
        // The universal bit is stripped; the MPT ledger flag survives.
        assert_eq!(issuance["Flags"].as_u64().unwrap(), 0x20);
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
