use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanPayTransactor;

impl Transactor for LoanPayTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_str_field(ctx.tx, "LoanBrokerOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanSequence").ok_or(TransactionResult::TemMalformed)?;

        let amount = helpers::get_u64_str_field(ctx.tx, "PaymentAmount")
            .ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, account_obj) = helpers::read_account_by_address(ctx.view, account_str)?;

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

        // Caller must be Borrower
        let borrower = loan["Borrower"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if borrower != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // Status must be 0 (Active)
        let status = loan["Status"].as_u64().unwrap_or(0);
        if status != 0 {
            return Err(TransactionResult::TecNoPermission);
        }

        // Sufficient balance
        let payment_amount = helpers::get_u64_str_field(ctx.tx, "PaymentAmount")
            .ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&account_obj);
        if balance < payment_amount + fee {
            return Err(TransactionResult::TecUnfundedPayment);
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
        let payment_amount = helpers::get_u64_str_field(ctx.tx, "PaymentAmount")
            .ok_or(TransactionResult::TemBadAmount)?;

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
        let total_value_outstanding: u64 = loan["TotalValueOutstanding"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let interest_accrued: u64 = loan["InterestAccrued"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let loan_rate = loan["LoanRate"].as_u64().unwrap_or(0);
        let mgmt_fee_rate = loan["ManagementFeeRate"].as_u64().unwrap_or(0);

        // Time-based interest accrual since last payment (or loan start)
        let last_payment = loan["LastPaymentDate"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0) as u32;
        let loan_start = loan["LoanMaturityDate"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0) as u32;
        let base_time = if last_payment > 0 { last_payment } else { loan_start };

        // Use parent ledger close time as "now"
        let current_time = ctx.view.parent_close_time();

        // Days elapsed (minimum 1 to avoid zero interest)
        let seconds_elapsed = current_time.saturating_sub(base_time);
        let days_elapsed = (seconds_elapsed / 86400).max(1) as u64;

        // Annual rate in parts per million -> daily rate applied over elapsed days
        // interest = principal * rate * days / (1_000_000 * 365)
        let new_interest = principal_outstanding
            .checked_mul(loan_rate)
            .and_then(|v| v.checked_mul(days_elapsed))
            .ok_or(TransactionResult::TefInternal)?
            / (1_000_000 * 365);
        let total_interest = interest_accrued + new_interest;

        // Calculate management fee on interest
        let mgmt_fee = new_interest
            .checked_mul(mgmt_fee_rate)
            .ok_or(TransactionResult::TefInternal)?
            / 10000;

        // Apply payment: first to interest + fees, then to principal
        let mut remaining = payment_amount;

        // Pay management fee first
        let fee_paid = remaining.min(mgmt_fee);
        remaining -= fee_paid;

        // Pay interest
        let interest_paid = remaining.min(total_interest);
        remaining -= interest_paid;

        // Pay principal
        let principal_paid = remaining.min(principal_outstanding);
        remaining -= principal_paid;

        let new_principal = principal_outstanding - principal_paid;
        let new_interest_remaining = total_interest - interest_paid;
        let new_total_value = total_value_outstanding
            .saturating_sub(payment_amount - remaining);

        // Update loan
        loan["PrincipalOutstanding"] = serde_json::Value::String(new_principal.to_string());
        loan["TotalValueOutstanding"] = serde_json::Value::String(new_total_value.to_string());
        loan["InterestAccrued"] = serde_json::Value::String(new_interest_remaining.to_string());
        loan["LastPaymentDate"] =
            serde_json::Value::String((ctx.view.parent_close_time() as u64).to_string());

        let loan_data = serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(loan_key, loan_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update broker DebtTotal
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        broker["DebtTotal"] =
            serde_json::Value::String(debt_total.saturating_sub(principal_paid).to_string());

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit vault with the actual payment amount (minus any overpayment)
        let effective_payment = payment_amount - remaining;

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
            serde_json::Value::String((total_deposited + effective_payment).to_string());

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Debit borrower
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&account);
        helpers::set_balance(
            &mut account,
            balance
                .checked_sub(effective_payment)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
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
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const BORROWER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_loan() -> Ledger {
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

        // Vault
        let owner_id = decode_account_id(OWNER).unwrap();
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
    fn valid_payment() {
        let ledger = setup_with_loan();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanPay",
            "Account": BORROWER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "PaymentAmount": "500000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanPayTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify borrower debited
        let borrower_id = decode_account_id(BORROWER).unwrap();
        let borrower_key = keylet::account(&borrower_id);
        let borrower_bytes = sandbox.read(&borrower_key).unwrap();
        let borrower: serde_json::Value = serde_json::from_slice(&borrower_bytes).unwrap();
        let borrower_balance: u64 = borrower["Balance"].as_str().unwrap().parse().unwrap();
        assert!(borrower_balance < 50_000_000);
    }

    #[test]
    fn partial_payment() {
        let ledger = setup_with_loan();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanPay",
            "Account": BORROWER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "PaymentAmount": "100000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanPayTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Principal should still be > 0
        let owner_id = decode_account_id(OWNER).unwrap();
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        let loan_bytes = sandbox.read(&loan_key).unwrap();
        let loan: serde_json::Value = serde_json::from_slice(&loan_bytes).unwrap();
        let principal: u64 = loan["PrincipalOutstanding"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(principal > 0);
    }

    #[test]
    fn full_repayment() {
        let ledger = setup_with_loan();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        // Pay more than total outstanding to fully repay
        let tx = serde_json::json!({
            "TransactionType": "LoanPay",
            "Account": BORROWER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "PaymentAmount": "10000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanPayTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Principal should be 0
        let owner_id = decode_account_id(OWNER).unwrap();
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        let loan_bytes = sandbox.read(&loan_key).unwrap();
        let loan: serde_json::Value = serde_json::from_slice(&loan_bytes).unwrap();
        assert_eq!(loan["PrincipalOutstanding"].as_str().unwrap(), "0");
    }

    #[test]
    fn reject_zero_payment() {
        let tx = serde_json::json!({
            "TransactionType": "LoanPay",
            "Account": BORROWER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "LoanSequence": 1,
            "PaymentAmount": "0",
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
            LoanPayTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }
}
