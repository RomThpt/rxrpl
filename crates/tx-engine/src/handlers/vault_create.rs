use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultCreateTransactor;

impl Transactor for VaultCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Asset is required
        let asset = ctx.tx.get("Asset").ok_or(TransactionResult::TemMalformed)?;

        // Asset must be "XRP" string or an IOU object with currency+issuer
        if let Some(s) = asset.as_str() {
            if s != "XRP" {
                return Err(TransactionResult::TemMalformed);
            }
        } else if asset.is_object() {
            asset
                .get("currency")
                .and_then(|v| v.as_str())
                .filter(|c| !c.is_empty())
                .ok_or(TransactionResult::TemBadCurrency)?;
            asset
                .get("issuer")
                .and_then(|v| v.as_str())
                .ok_or(TransactionResult::TemBadIssuer)?;
        } else {
            return Err(TransactionResult::TemMalformed);
        }

        // If MaxDeposit present, must be > 0
        if let Some(max_deposit) = helpers::get_u64_str_field(ctx.tx, "MaxDeposit") {
            if max_deposit == 0 {
                return Err(TransactionResult::TemBadAmount);
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

        // Read and update account
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let seq = helpers::get_sequence(&account);

        // Build vault entry
        let asset = ctx.tx.get("Asset").unwrap().clone();
        let mut vault = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": account_str,
            "Sequence": seq,
            "Asset": asset,
            "TotalDeposited": "0",
            "TotalShares": "0",
            "Flags": 0,
        });

        if let Some(max_deposit) = helpers::get_u64_str_field(ctx.tx, "MaxDeposit") {
            vault["MaxDeposit"] = serde_json::Value::String(max_deposit.to_string());
        }

        let vault_key = keylet::vault(&account_id, seq);
        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update account: increment sequence, +1 owner count
        helpers::increment_sequence(&mut account);
        helpers::adjust_owner_count(&mut account, 1);

        let acct_data = serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
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

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
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
    fn create_xrp_vault() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "XRP",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify vault exists
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["LedgerEntryType"].as_str().unwrap(), "Vault");
        assert_eq!(vault["Owner"].as_str().unwrap(), OWNER);
        assert_eq!(vault["Asset"].as_str().unwrap(), "XRP");
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "0");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "0");
        assert_eq!(vault["Sequence"].as_u64().unwrap(), 1);

        // Verify owner count incremented
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn create_vault_with_max_deposit() {
        let ledger = setup_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "XRP",
            "MaxDeposit": "50000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["MaxDeposit"].as_str().unwrap(), "50000000");
    }

    #[test]
    fn reject_missing_asset() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
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
            VaultCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_invalid_asset_string() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "BTC",
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
            VaultCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_zero_max_deposit() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": "XRP",
            "MaxDeposit": "0",
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
            VaultCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn accept_iou_asset() {
        let tx = serde_json::json!({
            "TransactionType": "VaultCreate",
            "Account": OWNER,
            "Asset": {
                "currency": "USD",
                "issuer": "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN"
            },
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(VaultCreateTransactor.preflight(&ctx), Ok(()));
    }
}
