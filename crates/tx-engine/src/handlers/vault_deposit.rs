use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultDepositTransactor;

impl Transactor for VaultDepositTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "VaultOwner").ok_or(TransactionResult::TemMalformed)?;

        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let depositor_str = helpers::get_account(ctx.tx)?;
        let (_, depositor_acct) = helpers::read_account_by_address(ctx.view, depositor_str)?;

        let vault_owner_str =
            helpers::get_str_field(ctx.tx, "VaultOwner").ok_or(TransactionResult::TemMalformed)?;
        let vault_seq =
            helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;

        let vault_owner_id = decode_account_id(vault_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let vault_key = keylet::vault(&vault_owner_id, vault_seq);

        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&depositor_acct);

        // Depositor must have sufficient XRP balance (Amount + fee as reserve proxy)
        if balance < amount + fee {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        // If MaxDeposit set, TotalDeposited + Amount <= MaxDeposit
        let total_deposited: u64 = vault["TotalDeposited"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if let Some(max_deposit_str) = vault.get("MaxDeposit").and_then(|v| v.as_str()) {
            if let Ok(max_deposit) = max_deposit_str.parse::<u64>() {
                if total_deposited + amount > max_deposit {
                    return Err(TransactionResult::TecOversize);
                }
            }
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let depositor_str = helpers::get_account(ctx.tx)?;
        let depositor_id =
            decode_account_id(depositor_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let vault_owner_str = helpers::get_str_field(ctx.tx, "VaultOwner")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let vault_seq =
            helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;
        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;

        let vault_owner_id = decode_account_id(&vault_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let vault_key = keylet::vault(&vault_owner_id, vault_seq);

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
        let total_shares: u64 = vault["TotalShares"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Compute shares
        let shares = if total_shares == 0 {
            amount
        } else {
            amount
                .checked_mul(total_shares)
                .ok_or(TransactionResult::TefInternal)?
                / total_deposited
        };

        // Update vault
        vault["TotalDeposited"] =
            serde_json::Value::String((total_deposited + amount).to_string());
        vault["TotalShares"] =
            serde_json::Value::String((total_shares + shares).to_string());

        let vault_data =
            serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Deduct Amount from depositor's XRP balance and increment sequence
        let depositor_key = keylet::account(&depositor_id);
        let depositor_bytes = ctx
            .view
            .read(&depositor_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut depositor_acct: serde_json::Value =
            serde_json::from_slice(&depositor_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&depositor_acct);
        helpers::set_balance(
            &mut depositor_acct,
            balance
                .checked_sub(amount)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
        helpers::increment_sequence(&mut depositor_acct);

        let depositor_data =
            serde_json::to_vec(&depositor_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(depositor_key, depositor_data)
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
    const DEPOSITOR: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (DEPOSITOR, 50_000_000)] {
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
        ledger
    }

    fn setup_with_vault() -> (Ledger, rxrpl_primitives::Hash256) {
        let mut ledger = setup_accounts();
        let owner_id = decode_account_id(OWNER).unwrap();
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

    fn setup_with_funded_vault() -> (Ledger, rxrpl_primitives::Hash256) {
        let mut ledger = setup_accounts();
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": OWNER,
            "Sequence": 1,
            "Asset": "XRP",
            "TotalDeposited": "10000000",
            "TotalShares": "10000000",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        (ledger, vault_key)
    }

    #[test]
    fn first_deposit_creates_shares_equal_to_amount() {
        let (ledger, vault_key) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "Amount": "5000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultDepositTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify vault updated
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "5000000");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "5000000");

        // Verify depositor balance decreased
        let dep_id = decode_account_id(DEPOSITOR).unwrap();
        let dep_key = keylet::account(&dep_id);
        let dep_bytes = sandbox.read(&dep_key).unwrap();
        let dep: serde_json::Value = serde_json::from_slice(&dep_bytes).unwrap();
        assert_eq!(dep["Balance"].as_str().unwrap(), "45000000");
        assert_eq!(dep["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn proportional_shares_on_second_deposit() {
        let (ledger, vault_key) = setup_with_funded_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "Amount": "5000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultDepositTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "15000000");
        // shares = 5000000 * 10000000 / 10000000 = 5000000
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "15000000");
    }

    #[test]
    fn reject_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
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
            VaultDepositTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_insufficient_balance() {
        let (ledger, _) = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "Amount": "60000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultDepositTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfundedPayment)
        );
    }

    #[test]
    fn reject_exceeds_max_deposit() {
        let mut ledger = setup_accounts();
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
        let entry = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": OWNER,
            "Sequence": 1,
            "Asset": "XRP",
            "TotalDeposited": "8000000",
            "TotalShares": "8000000",
            "MaxDeposit": "10000000",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "Amount": "3000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultDepositTransactor.preclaim(&ctx),
            Err(TransactionResult::TecOversize)
        );
    }

    #[test]
    fn reject_missing_vault_owner() {
        let tx = serde_json::json!({
            "TransactionType": "VaultDeposit",
            "Account": DEPOSITOR,
            "VaultSequence": 1,
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
            VaultDepositTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
