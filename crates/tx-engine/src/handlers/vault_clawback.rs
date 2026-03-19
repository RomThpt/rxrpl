use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultClawbackTransactor;

impl Transactor for VaultClawbackTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;

        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;

        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let vault_key = keylet::vault(&account_id, vault_seq);

        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Owner must match Account
        let owner = vault["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // TotalDeposited >= Amount
        let total_deposited: u64 = vault["TotalDeposited"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;

        if total_deposited < amount {
            return Err(TransactionResult::TecUnfunded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;

        let vault_key = keylet::vault(&account_id, vault_seq);

        // Read vault
        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let total_deposited: u64 = vault["TotalDeposited"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Deduct from vault (TotalShares stays the same -- devalues shares)
        vault["TotalDeposited"] = serde_json::Value::String((total_deposited - amount).to_string());

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit Amount to owner's XRP balance and increment sequence
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance + amount);
        helpers::increment_sequence(&mut account);

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

    fn setup_with_funded_vault(
        total_deposited: u64,
        total_shares: u64,
    ) -> (Ledger, rxrpl_primitives::Hash256) {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&owner_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 2,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let vault_key = keylet::vault(&owner_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": OWNER,
            "Sequence": 1,
            "Asset": "XRP",
            "TotalDeposited": total_deposited.to_string(),
            "TotalShares": total_shares.to_string(),
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn clawback_partial() {
        let (ledger, vault_key) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": OWNER,
            "VaultSequence": 1,
            "Amount": "3000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Vault deposited reduced, shares unchanged (devalued)
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "7000000");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "10000000");

        // Owner credited
        let owner_id = decode_account_id(OWNER).unwrap();
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "103000000");
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 3);
    }

    #[test]
    fn clawback_total() {
        let (ledger, vault_key) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": OWNER,
            "VaultSequence": 1,
            "Amount": "10000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "0");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "10000000");
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": OWNER,
            "VaultSequence": 1,
            "Amount": "0",
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
            VaultClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_exceeds_total_deposited() {
        let (ledger, _) = setup_with_funded_vault(5_000_000, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": OWNER,
            "VaultSequence": 1,
            "Amount": "10000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultClawbackTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfunded)
        );
    }

    #[test]
    fn reject_missing_vault_sequence() {
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": OWNER,
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
        assert_eq!(
            VaultClawbackTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_non_owner_clawback() {
        let (ledger, _) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        // Non-owner account won't find vault keyed by their id
        let other = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
        let tx = serde_json::json!({
            "TransactionType": "VaultClawback",
            "Account": other,
            "VaultSequence": 1,
            "Amount": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        // OTHER's account doesn't exist
        assert!(VaultClawbackTransactor.preclaim(&ctx).is_err());
    }
}
