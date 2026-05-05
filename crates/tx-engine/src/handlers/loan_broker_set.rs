use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerSetTransactor;

impl Transactor for LoanBrokerSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // VaultID is required (VaultOwner + VaultSequence identify the vault)
        helpers::get_str_field(ctx.tx, "VaultOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "VaultSequence").ok_or(TransactionResult::TemMalformed)?;

        // ManagementFeeRate must be <= 10000 (basis points)
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "ManagementFeeRate") {
            if rate > 10000 {
                return Err(TransactionResult::TemMalformed);
            }
        } else {
            return Err(TransactionResult::TemMalformed);
        }

        // CoverRateMinimum must be <= 100000 (parts per million)
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "CoverRateMinimum") {
            if rate > 100000 {
                return Err(TransactionResult::TemMalformed);
            }
        } else {
            return Err(TransactionResult::TemMalformed);
        }

        // CoverRateLiquidation must be <= 100000
        if let Some(rate) = helpers::get_u32_field(ctx.tx, "CoverRateLiquidation") {
            if rate > 100000 {
                return Err(TransactionResult::TemMalformed);
            }
        } else {
            return Err(TransactionResult::TemMalformed);
        }

        // DebtMaximum required, must be > 0
        let debt_max = helpers::get_u64_str_field(ctx.tx, "DebtMaximum")
            .ok_or(TransactionResult::TemBadAmount)?;
        if debt_max == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        // Data field, if present, must be <= 256 bytes
        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            if data.len() > 256 {
                return Err(TransactionResult::TemMalformed);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Vault must exist and caller must be vault owner
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

        // Caller must be vault owner
        let vault_owner = vault["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if vault_owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

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
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let seq = helpers::get_sequence(&account);

        let vault_owner_str = helpers::get_str_field(ctx.tx, "VaultOwner")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let vault_seq = helpers::get_u32_field(ctx.tx, "VaultSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let debt_maximum = helpers::get_u64_str_field(ctx.tx, "DebtMaximum")
            .ok_or(TransactionResult::TemBadAmount)?;
        let cover_rate_min = helpers::get_u32_field(ctx.tx, "CoverRateMinimum")
            .ok_or(TransactionResult::TemMalformed)?;
        let cover_rate_liq = helpers::get_u32_field(ctx.tx, "CoverRateLiquidation")
            .ok_or(TransactionResult::TemMalformed)?;
        let mgmt_fee_rate = helpers::get_u32_field(ctx.tx, "ManagementFeeRate")
            .ok_or(TransactionResult::TemMalformed)?;

        // Build VaultID string for reference
        let vault_id = format!("{}:{}", vault_owner_str, vault_seq);

        // Build LoanBroker entry
        let mut broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Owner": account_str,
            "Account": account_str,
            "VaultID": vault_id,
            "LoanSequence": 1,
            "OwnerCount": 0,
            "DebtTotal": "0",
            "DebtMaximum": debt_maximum.to_string(),
            "CoverAvailable": "0",
            "CoverRateMinimum": cover_rate_min,
            "CoverRateLiquidation": cover_rate_liq,
            "ManagementFeeRate": mgmt_fee_rate,
            "Flags": 0,
        });

        if let Some(data) = helpers::get_str_field(ctx.tx, "Data") {
            broker["Data"] = serde_json::Value::String(data.to_string());
        }

        let broker_key = keylet::loan_broker(account_id.as_bytes(), seq);
        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &broker_key)?;

        // Update account: increment sequence, +2 owner count
        helpers::increment_sequence(&mut account);
        helpers::adjust_owner_count(&mut account, 2);

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
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_with_vault() -> Ledger {
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

        // Create vault owned by OWNER
        let vault_key = keylet::vault(&id, 1);
        let vault = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": OWNER,
            "Sequence": 1,
            "Asset": "XRP",
            "TotalDeposited": "50000000",
            "TotalShares": "50000000",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&vault).unwrap())
            .unwrap();
        ledger
    }

    fn base_tx() -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "LoanBrokerSet",
            "Account": OWNER,
            "VaultOwner": OWNER,
            "VaultSequence": 1,
            "DebtMaximum": "10000000",
            "CoverRateMinimum": 50000,
            "CoverRateLiquidation": 80000,
            "ManagementFeeRate": 500,
            "Fee": "12",
            "Sequence": 1,
        })
    }

    #[test]
    fn reject_missing_vault_id() {
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerSet",
            "Account": OWNER,
            "DebtMaximum": "10000000",
            "CoverRateMinimum": 50000,
            "CoverRateLiquidation": 80000,
            "ManagementFeeRate": 500,
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
            LoanBrokerSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn reject_invalid_management_fee_rate() {
        let mut tx = base_tx();
        tx["ManagementFeeRate"] = serde_json::json!(20000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            LoanBrokerSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn valid_create() {
        let ledger = setup_with_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = base_tx();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanBrokerSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify broker entry exists
        let owner_id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["LedgerEntryType"].as_str().unwrap(), "LoanBroker");
        assert_eq!(broker["Owner"].as_str().unwrap(), OWNER);
        assert_eq!(broker["DebtTotal"].as_str().unwrap(), "0");
        assert_eq!(broker["LoanSequence"].as_u64().unwrap(), 1);
        assert_eq!(broker["OwnerCount"].as_u64().unwrap(), 0);
        assert_eq!(broker["CoverAvailable"].as_str().unwrap(), "0");

        // Verify owner count incremented by 2
        let acct_key = keylet::account(&owner_id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 2);
        assert_eq!(acct["Sequence"].as_u64().unwrap(), 2);
    }

    #[test]
    fn reject_data_too_long() {
        let mut tx = base_tx();
        tx["Data"] = serde_json::Value::String("X".repeat(300));
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            LoanBrokerSetTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }
}
