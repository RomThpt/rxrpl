use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanManageTransactor;

impl Transactor for LoanManageTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_str_field(ctx.tx, "LoanBrokerOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanSequence").ok_or(TransactionResult::TemMalformed)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let broker_owner_str = helpers::get_str_field(ctx.tx, "LoanBrokerOwner")
            .ok_or(TransactionResult::TemMalformed)?;
        let _broker_seq = helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let loan_seq =
            helpers::get_u32_field(ctx.tx, "LoanSequence").ok_or(TransactionResult::TemMalformed)?;

        let broker_owner_id = decode_account_id(broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let loan_key = keylet::loan(broker_owner_id.as_bytes(), loan_seq);

        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Caller must be Owner (broker operator)
        let owner = loan["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // Status must be 0 (Active)
        let status = loan["Status"].as_u64().unwrap_or(0);
        if status != 0 {
            return Err(TransactionResult::TecNoPermission);
        }

        // current_time > LoanMaturityDate + GracePeriodDays * 86400
        let maturity_date: u64 = loan["LoanMaturityDate"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let grace_days = loan["GracePeriodDays"].as_u64().unwrap_or(0);
        let deadline = maturity_date + grace_days * 86400;

        let current_time = ctx.view.parent_close_time() as u64;
        if current_time <= deadline {
            return Err(TransactionResult::TecTooSoon);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;

        let broker_owner_str = helpers::get_str_field(ctx.tx, "LoanBrokerOwner")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let broker_seq = helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        let loan_seq =
            helpers::get_u32_field(ctx.tx, "LoanSequence").ok_or(TransactionResult::TemMalformed)?;

        let broker_owner_id = decode_account_id(&broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let loan_key = keylet::loan(broker_owner_id.as_bytes(), loan_seq);

        // Read loan
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let principal_outstanding: u64 = loan["PrincipalOutstanding"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Set Status = 1 (Defaulted)
        loan["Status"] = serde_json::Value::from(1);

        let loan_data = serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(loan_key, loan_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Read broker for liquidation
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let cover_available: u64 = broker["CoverAvailable"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let cover_rate_liq = broker["CoverRateLiquidation"].as_u64().unwrap_or(0);

        // Liquidate cover: min(CoverRateLiquidation * principal / 1_000_000, CoverAvailable)
        let liquidation_max = cover_rate_liq
            .checked_mul(principal_outstanding)
            .ok_or(TransactionResult::TefInternal)?
            / 1_000_000;
        let liquidation = liquidation_max.min(cover_available);

        // Decrement CoverAvailable
        broker["CoverAvailable"] =
            serde_json::Value::String((cover_available - liquidation).to_string());

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit vault with liquidation amount
        if liquidation > 0 {
            let vault_id_str = broker["VaultID"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?
                .to_string();
            let parts: Vec<&str> = vault_id_str.split(':').collect();
            if parts.len() != 2 {
                return Err(TransactionResult::TefInternal);
            }
            let vault_owner_id =
                decode_account_id(parts[0]).map_err(|_| TransactionResult::TefInternal)?;
            let vault_seq: u32 = parts[1].parse().map_err(|_| TransactionResult::TefInternal)?;
            let vault_key = keylet::vault(&vault_owner_id, vault_seq);

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
            vault["TotalDeposited"] =
                serde_json::Value::String((total_deposited + liquidation).to_string());

            let vault_data =
                serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(vault_key, vault_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Update owner account sequence
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut account);

        let acct_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
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
    use crate::transactor::{ApplyContext, PreclaimContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BORROWER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_loan(maturity: &str, grace_days: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (BORROWER, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 2,
                "OwnerCount": 1,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let owner_id = decode_account_id(OWNER).unwrap();

        // Vault
        let vault_key = keylet::vault(&owner_id, 1);
        let vault = serde_json::json!({
            "LedgerEntryType": "Vault",
            "Owner": OWNER,
            "Sequence": 1,
            "Asset": "XRP",
            "TotalDeposited": "40000000",
            "TotalShares": "50000000",
            "Flags": 0,
        });
        ledger
            .put_state(vault_key, serde_json::to_vec(&vault).unwrap())
            .unwrap();

        // Broker
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Owner": OWNER,
            "Account": OWNER,
            "VaultID": format!("{}:1", OWNER),
            "LoanSequence": 2,
            "OwnerCount": 1,
            "DebtTotal": "5000000",
            "DebtMaximum": "20000000",
            "CoverAvailable": "5000000",
            "CoverRateMinimum": 50000,
            "CoverRateLiquidation": 80000,
            "ManagementFeeRate": 500,
            "Flags": 0,
        });
        ledger
            .put_state(broker_key, serde_json::to_vec(&broker).unwrap())
            .unwrap();

        // Loan
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        let loan = serde_json::json!({
            "LedgerEntryType": "Loan",
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "Borrower": BORROWER,
            "Owner": OWNER,
            "LoanPrincipal": "5000000",
            "PrincipalOutstanding": "5000000",
            "TotalValueOutstanding": "5500000",
            "InterestAccrued": "0",
            "LoanRate": 5000,
            "LoanPeriods": 12,
            "PeriodicPayment": "458333",
            "LoanMaturityDate": maturity,
            "OriginationFeeRate": 100,
            "ManagementFeeRate": 500,
            "GracePeriodDays": grace_days,
            "LastPaymentDate": "0",
            "Status": 0,
            "Flags": 0,
        });
        ledger
            .put_state(loan_key, serde_json::to_vec(&loan).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn valid_default() {
        // Maturity 100, grace 1 day = 86400, deadline = 86500
        // Set parent_close_time > 86500
        let mut ledger = setup_with_loan("100", 1);
        ledger.header.parent_close_time = 90000;

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanManage",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanManageTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify loan status = 1
        let owner_id = decode_account_id(OWNER).unwrap();
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        let loan_bytes = sandbox.read(&loan_key).unwrap();
        let loan: serde_json::Value = serde_json::from_slice(&loan_bytes).unwrap();
        assert_eq!(loan["Status"].as_u64().unwrap(), 1);

        // Verify cover liquidated
        // liquidation = min(80000 * 5000000 / 1000000, 5000000) = min(400000, 5000000) = 400000
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["CoverAvailable"].as_str().unwrap(), "4600000");
    }

    #[test]
    fn premature_default_rejected() {
        // Maturity far in the future
        let mut ledger = setup_with_loan("1000000", 30);
        ledger.header.parent_close_time = 100; // Way before deadline

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanManage",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanManageTransactor.preclaim(&ctx),
            Err(TransactionResult::TecTooSoon)
        );
    }

    #[test]
    fn non_owner_rejected() {
        let mut ledger = setup_with_loan("100", 0);
        ledger.header.parent_close_time = 200;

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanManage",
            "Account": BORROWER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanManageTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }
}
