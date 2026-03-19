use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultSetTransactor;

impl Transactor for VaultSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;
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

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let vault_key = keylet::vault(&account_id, vault_seq);

        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Update MaxDeposit if provided
        if let Some(max_deposit) = helpers::get_u64_str_field(ctx.tx, "MaxDeposit") {
            vault["MaxDeposit"] = serde_json::Value::String(max_deposit.to_string());
        }

        // Update Flags if provided
        if let Some(flags) = helpers::get_u32_field(ctx.tx, "Flags") {
            vault["Flags"] = serde_json::Value::from(flags);
        }

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment account sequence
        let acct_key = keylet::account(&account_id);
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
    const OTHER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_vault() -> (Ledger, rxrpl_primitives::Hash256) {
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
            "TotalDeposited": "0",
            "TotalShares": "0",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn update_max_deposit() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultSequence": 1,
            "MaxDeposit": "75000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["MaxDeposit"].as_str().unwrap(), "75000000");
    }

    #[test]
    fn update_flags() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultSequence": 1,
            "Flags": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["Flags"].as_u64().unwrap(), 1);
    }

    #[test]
    fn reject_missing_vault_sequence() {
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
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
            VaultSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_nonexistent_vault() {
        let mut ledger = Ledger::genesis();
        let owner_id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&owner_id);
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

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultSequence": 99,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_non_owner() {
        let (mut ledger, _) = setup_with_vault();
        // Add OTHER account
        let other_id = decode_account_id(OTHER).unwrap();
        let other_key = keylet::account(&other_id);
        let other_acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OTHER,
            "Balance": "50000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(other_key, serde_json::to_vec(&other_acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        // OTHER tries to modify OWNER's vault -- uses OWNER's vault_seq but keyed on OTHER
        // Actually, vault is keyed by owner+seq, so OTHER can't even find it via their own id.
        // The preclaim looks up vault by Account (OTHER) + VaultSequence, which won't exist.
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OTHER,
            "VaultSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn increments_account_sequence() {
        let (ledger, _) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultSet",
            "Account": OWNER,
            "VaultSequence": 1,
            "MaxDeposit": "100000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        VaultSetTransactor.apply(&mut ctx).unwrap();

        let owner_id = decode_account_id(OWNER).unwrap();
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 3);
    }
}
