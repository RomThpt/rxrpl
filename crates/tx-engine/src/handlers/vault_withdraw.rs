use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct VaultWithdrawTransactor;

impl Transactor for VaultWithdrawTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "VaultOwner").ok_or(TransactionResult::TemMalformed)?;

        let shares = helpers::get_u64_str_field(ctx.tx, "SharesAmount")
            .ok_or(TransactionResult::TemBadAmount)?;
        if shares == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let vault_owner_str =
            helpers::get_str_field(ctx.tx, "VaultOwner").ok_or(TransactionResult::TemMalformed)?;
        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;

        let vault_owner_id = decode_account_id(vault_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let vault_key = keylet::vault(&vault_owner_id, vault_seq);

        let vault_bytes = ctx
            .view
            .read(&vault_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let vault: serde_json::Value =
            serde_json::from_slice(&vault_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let total_shares: u64 = vault["TotalShares"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let shares_amount = helpers::get_u64_str_field(ctx.tx, "SharesAmount")
            .ok_or(TransactionResult::TemBadAmount)?;

        if total_shares < shares_amount {
            return Err(TransactionResult::TecUnfunded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let vault_owner_str = helpers::get_str_field(ctx.tx, "VaultOwner")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let shares_amount = helpers::get_u64_str_field(ctx.tx, "SharesAmount")
            .ok_or(TransactionResult::TemBadAmount)?;

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

        // Compute payout = SharesAmount * TotalDeposited / TotalShares
        let payout = shares_amount
            .checked_mul(total_deposited)
            .ok_or(TransactionResult::TefInternal)?
            / total_shares;

        // Update vault
        vault["TotalDeposited"] = serde_json::Value::String((total_deposited - payout).to_string());
        vault["TotalShares"] =
            serde_json::Value::String((total_shares - shares_amount).to_string());

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit payout to withdrawer's XRP balance and increment sequence
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance + payout);
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
    const DEPOSITOR: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_funded_vault(
        total_deposited: u64,
        total_shares: u64,
    ) -> (Ledger, rxrpl_primitives::Hash256) {
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

        let owner_id = decode_account_id(OWNER).unwrap();
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
    fn withdraw_full_shares() {
        let (ledger, vault_key) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "SharesAmount": "10000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Vault should be empty
        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "0");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "0");

        // Depositor got credited
        let dep_id = decode_account_id(DEPOSITOR).unwrap();
        let dep_key = keylet::account(&dep_id);
        let dep_bytes = sandbox.read(&dep_key).unwrap();
        let dep: serde_json::Value = serde_json::from_slice(&dep_bytes).unwrap();
        assert_eq!(dep["Balance"].as_str().unwrap(), "60000000");
        assert_eq!(dep["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn withdraw_partial_shares() {
        let (ledger, vault_key) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "SharesAmount": "4000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "6000000");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "6000000");

        let dep_id = decode_account_id(DEPOSITOR).unwrap();
        let dep_key = keylet::account(&dep_id);
        let dep_bytes = sandbox.read(&dep_key).unwrap();
        let dep: serde_json::Value = serde_json::from_slice(&dep_bytes).unwrap();
        assert_eq!(dep["Balance"].as_str().unwrap(), "54000000");
    }

    #[test]
    fn reject_zero_shares() {
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "SharesAmount": "0",
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
            VaultWithdrawTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn reject_insufficient_shares() {
        let (ledger, _) = setup_with_funded_vault(10_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "SharesAmount": "20000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            VaultWithdrawTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfunded)
        );
    }

    #[test]
    fn reject_missing_vault_owner() {
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultSequence": 1,
            "SharesAmount": "1000000",
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
            VaultWithdrawTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn withdraw_after_clawback_gets_devalued_shares() {
        // After a clawback devalues shares, withdrawer gets less
        let (ledger, vault_key) = setup_with_funded_vault(5_000_000, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "VaultWithdraw",
            "Account": DEPOSITOR,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "SharesAmount": "10000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = VaultWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // payout = 10000000 * 5000000 / 10000000 = 5000000
        let dep_id = decode_account_id(DEPOSITOR).unwrap();
        let dep_key = keylet::account(&dep_id);
        let dep_bytes = sandbox.read(&dep_key).unwrap();
        let dep: serde_json::Value = serde_json::from_slice(&dep_bytes).unwrap();
        assert_eq!(dep["Balance"].as_str().unwrap(), "55000000");

        let vault_bytes = sandbox.read(&vault_key).unwrap();
        let vault: serde_json::Value = serde_json::from_slice(&vault_bytes).unwrap();
        assert_eq!(vault["TotalDeposited"].as_str().unwrap(), "0");
        assert_eq!(vault["TotalShares"].as_str().unwrap(), "0");
    }
}
