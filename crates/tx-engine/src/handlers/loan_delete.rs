use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanDeleteTransactor;

impl Transactor for LoanDeleteTransactor {
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

        // Caller must be Owner or Borrower
        let owner = loan["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let borrower = loan["Borrower"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if account_str != owner && account_str != borrower {
            return Err(TransactionResult::TecNoPermission);
        }

        // PrincipalOutstanding must be "0"
        let principal: u64 = loan["PrincipalOutstanding"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if principal != 0 {
            return Err(TransactionResult::TecHasObligations);
        }

        // TotalValueOutstanding must be "0"
        let total_value: u64 = loan["TotalValueOutstanding"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if total_value != 0 {
            return Err(TransactionResult::TecHasObligations);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

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

        // Read loan to get borrower
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let borrower_str = loan["Borrower"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        // Delete loan entry
        ctx.view
            .erase(&loan_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Decrement LoanBroker.OwnerCount
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let broker_owner_count = broker["OwnerCount"].as_u64().unwrap_or(0);
        broker["OwnerCount"] = serde_json::Value::from(broker_owner_count.saturating_sub(1));

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // adjust_owner_count(-1) for borrower
        let borrower_id =
            decode_account_id(&borrower_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let borrower_key = keylet::account(&borrower_id);
        let borrower_bytes = ctx
            .view
            .read(&borrower_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut borrower_acct: serde_json::Value =
            serde_json::from_slice(&borrower_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut borrower_acct, -1);

        let borrower_data =
            serde_json::to_vec(&borrower_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(borrower_key, borrower_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update caller account sequence
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

    fn setup_with_loan(principal: &str, total_value: &str) -> Ledger {
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

        // Broker
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Owner": OWNER,
            "Account": OWNER,
            "VaultID": format!("{}:1", OWNER),
            "LoanSequence": 2,
            "OwnerCount": 1,
            "DebtTotal": principal,
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
            "PrincipalOutstanding": principal,
            "TotalValueOutstanding": total_value,
            "InterestAccrued": "0",
            "LoanRate": 5000,
            "LoanPeriods": 12,
            "PeriodicPayment": "458333",
            "LoanMaturityDate": "1000000",
            "OriginationFeeRate": 100,
            "ManagementFeeRate": 500,
            "GracePeriodDays": 30,
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
    fn outstanding_balance_prevents_delete() {
        let ledger = setup_with_loan("5000000", "5500000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanDelete",
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
            LoanDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }

    #[test]
    fn valid_delete() {
        let ledger = setup_with_loan("0", "0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanDelete",
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

        let result = LoanDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Loan entry should be gone
        let owner_id = decode_account_id(OWNER).unwrap();
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        assert!(sandbox.read(&loan_key).is_none());

        // Broker OwnerCount decremented
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["OwnerCount"].as_u64().unwrap(), 0);

        // Borrower owner count decremented
        let borrower_id = decode_account_id(BORROWER).unwrap();
        let borrower_key = keylet::account(&borrower_id);
        let borrower_bytes = sandbox.read(&borrower_key).unwrap();
        let borrower: serde_json::Value = serde_json::from_slice(&borrower_bytes).unwrap();
        assert_eq!(borrower["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn borrower_can_delete() {
        let ledger = setup_with_loan("0", "0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanDelete",
            "Account": BORROWER,
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

        let result = LoanDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }
}
