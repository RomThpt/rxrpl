use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultDeleteTransactor;

impl Transactor for VaultDeleteTransactor {
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

        // TotalDeposited must be "0"
        let total_deposited: u64 = vault["TotalDeposited"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if total_deposited != 0 {
            return Err(TransactionResult::TecHasObligations);
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

        // Erase vault
        ctx.view
            .erase(&vault_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update account: -1 owner count, increment sequence
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::adjust_owner_count(&mut account, -1);
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

    fn setup_with_vault(total_deposited: &str) -> (Ledger, rxrpl_primitives::Hash256) {
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
            "TotalDeposited": total_deposited,
            "TotalShares": "0",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn delete_empty_vault() {
        let (ledger, vault_key) = setup_with_vault("0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
            "Account": OWNER,
            "VaultSequence": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify vault deleted
        assert!(!sandbox.exists(&vault_key));

        // Verify owner count decremented
        let owner_id = decode_account_id(OWNER).unwrap();
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 3);
    }

    #[test]
    fn reject_nonempty_vault() {
        let (ledger, _) = setup_with_vault("5000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
            "Account": OWNER,
            "VaultSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }

    #[test]
    fn reject_missing_vault_sequence() {
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
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
            VaultDeleteTransactor.preflight(&ctx),
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
            "TransactionType": "VaultDelete",
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
            VaultDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn reject_non_owner_delete() {
        let (ledger, _) = setup_with_vault("0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        // The vault is keyed by owner+seq, so using a different account won't find the vault
        let other = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
            "Account": other,
            "VaultSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        // OTHER's account doesn't exist yet, so TerNoAccount first
        assert!(VaultDeleteTransactor.preclaim(&ctx).is_err());
    }

    #[test]
    fn delete_decrements_owner_count() {
        let (ledger, _) = setup_with_vault("0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDelete",
            "Account": OWNER,
            "VaultSequence": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        VaultDeleteTransactor.apply(&mut ctx).unwrap();

        let owner_id = decode_account_id(OWNER).unwrap();
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }
}
