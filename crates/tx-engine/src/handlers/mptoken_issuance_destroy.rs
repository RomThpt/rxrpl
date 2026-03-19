use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct MPTokenIssuanceDestroyTransactor;

/// Parse MPTokenIssuanceID hex string into a Hash256 key.
fn parse_issuance_id(tx: &Value) -> Result<Hash256, TransactionResult> {
    let hex_str =
        helpers::get_str_field(tx, "MPTokenIssuanceID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Hash256::new(arr))
}

impl Transactor for MPTokenIssuanceDestroyTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if helpers::get_str_field(ctx.tx, "MPTokenIssuanceID").is_none() {
            return Err(TransactionResult::TemMalformed);
        }
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let issuance_key = parse_issuance_id(ctx.tx)?;
        let issuance_bytes = ctx
            .view
            .read(&issuance_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let issuance: Value =
            serde_json::from_slice(&issuance_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Issuer must match Account
        let issuer = issuance["Issuer"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if issuer != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // OutstandingAmount must be "0"
        let outstanding = issuance["OutstandingAmount"].as_str().unwrap_or("0");
        if outstanding != "0" {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let issuance_key = parse_issuance_id(ctx.tx)?;

        // Erase issuance
        ctx.view
            .erase(&issuance_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, -1);

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
    const HOLDER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(ISSUER, 100_000_000u64), (HOLDER, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 1,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        ledger
    }

    fn setup_with_issuance() -> (Ledger, Hash256) {
        let mut ledger = setup_accounts();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": ISSUER,
            "Sequence": 1,
            "MaximumAmount": "1000000",
            "TransferFee": 0,
            "AssetScale": 2,
            "OutstandingAmount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(issuance_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, issuance_key)
    }

    fn issuance_id_hex(key: &Hash256) -> String {
        hex::encode(key.as_bytes()).to_uppercase()
    }

    #[test]
    fn destroy_issuance_success() {
        let (ledger, issuance_key) = setup_with_issuance();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = MPTokenIssuanceDestroyTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify issuance erased
        assert!(sandbox.read(&issuance_key).is_none());

        // Verify owner count decremented
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let acct_key = keylet::account(&issuer_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn reject_outstanding_supply() {
        let mut ledger = setup_accounts();
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuance_key = keylet::mptoken_issuance(&issuer_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "MPTokenIssuance",
            "Issuer": ISSUER,
            "Sequence": 1,
            "OutstandingAmount": "500",
            "Flags": 0,
        });
        ledger
            .put_state(issuance_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": ISSUER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceDestroyTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn reject_wrong_issuer() {
        let (ledger, issuance_key) = setup_with_issuance();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": HOLDER,
            "MPTokenIssuanceID": issuance_id_hex(&issuance_key),
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceDestroyTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn reject_missing_issuance_id() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": ISSUER,
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
            MPTokenIssuanceDestroyTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_nonexistent_issuance() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": ISSUER,
            "MPTokenIssuanceID": "0000000000000000000000000000000000000000000000000000000000000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            MPTokenIssuanceDestroyTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_invalid_hex() {
        let tx = serde_json::json!({
            "TransactionType": "MPTokenIssuanceDestroy",
            "Account": ISSUER,
            "MPTokenIssuanceID": "ZZZZ",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        // preflight passes (only checks presence), but parse will fail in preclaim
        assert_eq!(MPTokenIssuanceDestroyTransactor.preflight(&ctx), Ok(()));
    }
}
