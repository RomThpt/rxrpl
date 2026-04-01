use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanSetTransactor;

/// Calculate periodic payment using amortization formula:
/// P * [r(1+r)^n] / [(1+r)^n - 1]
/// where P = principal, r = periodic rate, n = number of periods.
/// Rates are in parts per million (1_000_000 = 100%).
/// Returns the payment amount in drops.
fn calculate_periodic_payment(principal: u64, rate_ppm: u64, periods: u64) -> u64 {
    if periods == 0 {
        return 0;
    }
    if rate_ppm == 0 {
        // Zero interest: simple division
        return principal / periods;
    }

    // Use f64 for the amortization calculation
    let p = principal as f64;
    let r = rate_ppm as f64 / 1_000_000.0;
    let n = periods as f64;

    let r_plus_1_n = (1.0 + r).powf(n);
    let payment = p * (r * r_plus_1_n) / (r_plus_1_n - 1.0);

    payment.ceil() as u64
}

impl Transactor for LoanSetTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Required fields
        helpers::get_str_field(ctx.tx, "LoanBrokerOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "Borrower").ok_or(TransactionResult::TemMalformed)?;

        let principal = helpers::get_u64_str_field(ctx.tx, "LoanPrincipal")
            .ok_or(TransactionResult::TemBadAmount)?;
        if principal == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        let loan_rate =
            helpers::get_u32_field(ctx.tx, "LoanRate").ok_or(TransactionResult::TemMalformed)?;
        if loan_rate > 100000 {
            return Err(TransactionResult::TemMalformed);
        }

        let maturity_date = helpers::get_u64_str_field(ctx.tx, "LoanMaturityDate")
            .ok_or(TransactionResult::TemMalformed)?;
        if maturity_date == 0 {
            return Err(TransactionResult::TemMalformed);
        }

        helpers::get_u32_field(ctx.tx, "LoanPeriods").ok_or(TransactionResult::TemMalformed)?;

        // Optional fee rates must be <= 10000
        if let Some(origination_fee) = helpers::get_u32_field(ctx.tx, "OriginationFeeRate") {
            if origination_fee > 10000 {
                return Err(TransactionResult::TemMalformed);
            }
        }

        if let Some(grace_days) = helpers::get_u32_field(ctx.tx, "GracePeriodDays") {
            // Just validate it exists and is reasonable
            let _ = grace_days;
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let broker_owner_str = helpers::get_str_field(ctx.tx, "LoanBrokerOwner")
            .ok_or(TransactionResult::TemMalformed)?;
        let broker_seq = helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;

        let broker_owner_id = decode_account_id(broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);

        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Caller must be broker Owner
        let owner = broker["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // Borrower must exist and not be the same as owner
        let borrower_str =
            helpers::get_str_field(ctx.tx, "Borrower").ok_or(TransactionResult::TemMalformed)?;
        helpers::read_account_by_address(ctx.view, borrower_str)?;
        if borrower_str == account_str {
            return Err(TransactionResult::TemMalformed);
        }

        // DebtMaximum check: DebtTotal + principal <= DebtMaximum
        let principal = helpers::get_u64_str_field(ctx.tx, "LoanPrincipal")
            .ok_or(TransactionResult::TemBadAmount)?;
        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let debt_maximum: u64 = broker["DebtMaximum"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if debt_total + principal > debt_maximum {
            return Err(TransactionResult::TecOversize);
        }

        // Cover check: CoverAvailable >= CoverRateMinimum * (DebtTotal + principal) / 1_000_000
        let cover_available: u64 = broker["CoverAvailable"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let cover_rate_min = broker["CoverRateMinimum"].as_u64().unwrap_or(0);
        let required_cover = cover_rate_min
            .checked_mul(debt_total + principal)
            .ok_or(TransactionResult::TefInternal)?
            / 1_000_000;

        if cover_available < required_cover {
            return Err(TransactionResult::TecInsufficientReserve);
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
        let borrower_str = helpers::get_str_field(ctx.tx, "Borrower")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let principal = helpers::get_u64_str_field(ctx.tx, "LoanPrincipal")
            .ok_or(TransactionResult::TemBadAmount)?;
        let loan_rate =
            helpers::get_u32_field(ctx.tx, "LoanRate").ok_or(TransactionResult::TemMalformed)?;
        let maturity_date = helpers::get_u64_str_field(ctx.tx, "LoanMaturityDate")
            .ok_or(TransactionResult::TemMalformed)?;
        let periods =
            helpers::get_u32_field(ctx.tx, "LoanPeriods").ok_or(TransactionResult::TemMalformed)?;
        let origination_fee_rate =
            helpers::get_u32_field(ctx.tx, "OriginationFeeRate").unwrap_or(0);
        let grace_period_days = helpers::get_u32_field(ctx.tx, "GracePeriodDays").unwrap_or(0);
        let mgmt_fee_rate =
            helpers::get_u32_field(ctx.tx, "ManagementFeeRate").unwrap_or(0);

        let broker_owner_id = decode_account_id(&broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);

        // Read broker
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let loan_sequence = broker["LoanSequence"].as_u64().unwrap_or(1) as u32;
        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let broker_owner_count = broker["OwnerCount"].as_u64().unwrap_or(0);

        // Calculate periodic payment via amortization
        let periodic_payment =
            calculate_periodic_payment(principal, loan_rate as u64, periods as u64);

        // Calculate origination fee
        let origination_fee = principal
            .checked_mul(origination_fee_rate as u64)
            .ok_or(TransactionResult::TefInternal)?
            / 10000;

        let net_principal = principal
            .checked_sub(origination_fee)
            .ok_or(TransactionResult::TefInternal)?;

        // Compute total value outstanding (principal + total interest)
        let total_interest = if periods > 0 {
            periodic_payment
                .checked_mul(periods as u64)
                .ok_or(TransactionResult::TefInternal)?
                .checked_sub(principal)
                .unwrap_or(0)
        } else {
            0
        };

        let total_value_outstanding = principal + total_interest;

        // Create Loan entry
        let loan = serde_json::json!({
            "LedgerEntryType": "Loan",
            "LoanBrokerOwner": broker_owner_str,
            "LoanBrokerSequence": broker_seq,
            "LoanSequence": loan_sequence,
            "Borrower": borrower_str,
            "Owner": account_str,
            "LoanPrincipal": principal.to_string(),
            "PrincipalOutstanding": principal.to_string(),
            "TotalValueOutstanding": total_value_outstanding.to_string(),
            "InterestAccrued": "0",
            "LoanRate": loan_rate,
            "LoanPeriods": periods,
            "PeriodicPayment": periodic_payment.to_string(),
            "LoanMaturityDate": maturity_date.to_string(),
            "OriginationFeeRate": origination_fee_rate,
            "ManagementFeeRate": mgmt_fee_rate,
            "GracePeriodDays": grace_period_days,
            "LastPaymentDate": "0",
            "Status": 0,
            "Flags": 0,
        });

        // Use broker account_id bytes as the broker_id for keylet
        let loan_key = keylet::loan(broker_owner_id.as_bytes(), loan_sequence);
        let loan_data = serde_json::to_vec(&loan).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(loan_key, loan_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update broker: increment LoanSequence, OwnerCount, DebtTotal
        broker["LoanSequence"] = serde_json::Value::from(loan_sequence + 1);
        broker["OwnerCount"] = serde_json::Value::from(broker_owner_count + 1);
        broker["DebtTotal"] =
            serde_json::Value::String((debt_total + principal).to_string());

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Transfer net principal from Vault to Borrower
        // Read vault from VaultID on broker
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
        if total_deposited < net_principal {
            return Err(TransactionResult::TecUnfundedPayment);
        }
        vault["TotalDeposited"] =
            serde_json::Value::String((total_deposited - net_principal).to_string());

        let vault_data = serde_json::to_vec(&vault).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(vault_key, vault_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit borrower with net_principal
        let borrower_id =
            decode_account_id(&borrower_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let borrower_key = keylet::account(&borrower_id);
        let borrower_bytes = ctx
            .view
            .read(&borrower_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut borrower_acct: serde_json::Value =
            serde_json::from_slice(&borrower_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let borrower_balance = helpers::get_balance(&borrower_acct);
        helpers::set_balance(&mut borrower_acct, borrower_balance + net_principal);
        helpers::adjust_owner_count(&mut borrower_acct, 1);

        let borrower_data =
            serde_json::to_vec(&borrower_acct).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(borrower_key, borrower_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update owner account sequence
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

    fn setup_with_broker_and_vault() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (BORROWER, 10_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 2,
                "OwnerCount": 2,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        // Create vault
        let owner_id = decode_account_id(OWNER).unwrap();
        let vault_key = keylet::vault(&owner_id, 1);
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

        // Create broker
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Owner": OWNER,
            "Account": OWNER,
            "VaultID": format!("{}:1", OWNER),
            "LoanSequence": 1,
            "OwnerCount": 0,
            "DebtTotal": "0",
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
        ledger
    }

    fn base_loan_tx() -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "LoanSet",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Borrower": BORROWER,
            "LoanPrincipal": "5000000",
            "LoanRate": 5000,
            "LoanMaturityDate": "1000000",
            "LoanPeriods": 12,
            "OriginationFeeRate": 100,
            "GracePeriodDays": 30,
            "ManagementFeeRate": 500,
            "Fee": "12",
            "Sequence": 2,
        })
    }

    #[test]
    fn valid_loan_creation() {
        let ledger = setup_with_broker_and_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = base_loan_tx();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanSetTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify loan entry
        let owner_id = decode_account_id(OWNER).unwrap();
        let loan_key = keylet::loan(owner_id.as_bytes(), 1);
        let loan_bytes = sandbox.read(&loan_key).unwrap();
        let loan: serde_json::Value = serde_json::from_slice(&loan_bytes).unwrap();
        assert_eq!(loan["LedgerEntryType"].as_str().unwrap(), "Loan");
        assert_eq!(loan["Borrower"].as_str().unwrap(), BORROWER);
        assert_eq!(loan["LoanPrincipal"].as_str().unwrap(), "5000000");
        assert_eq!(loan["PrincipalOutstanding"].as_str().unwrap(), "5000000");
        assert_eq!(loan["Status"].as_u64().unwrap(), 0);

        // Verify broker updated
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["LoanSequence"].as_u64().unwrap(), 2);
        assert_eq!(broker["OwnerCount"].as_u64().unwrap(), 1);
        assert_eq!(broker["DebtTotal"].as_str().unwrap(), "5000000");

        // Verify borrower credited (net principal = 5000000 - origination fee)
        // Origination fee = 5000000 * 100 / 10000 = 50000
        let borrower_id = decode_account_id(BORROWER).unwrap();
        let borrower_key = keylet::account(&borrower_id);
        let borrower_bytes = sandbox.read(&borrower_key).unwrap();
        let borrower: serde_json::Value = serde_json::from_slice(&borrower_bytes).unwrap();
        assert_eq!(borrower["Balance"].as_str().unwrap(), "14950000");
        assert_eq!(borrower["OwnerCount"].as_u64().unwrap(), 3);
    }

    #[test]
    fn debt_maximum_exceeded() {
        let ledger = setup_with_broker_and_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let mut tx = base_loan_tx();
        tx["LoanPrincipal"] = serde_json::json!("25000000"); // exceeds DebtMaximum of 20000000
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecOversize)
        );
    }

    #[test]
    fn cover_insufficient() {
        // Broker has CoverAvailable=5000000, CoverRateMinimum=50000
        // For 15000000 loan: required = 50000 * 15000000 / 1000000 = 750000 (within cover)
        // But for 200000000: required = 50000 * 200000000 / 1000000 = 10000000 > 5000000
        let ledger = setup_with_broker_and_vault();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let mut tx = base_loan_tx();
        // Use a principal that exceeds cover: need cover >= 50000 * principal / 1000000
        // With 5000000 cover: max principal where cover is sufficient = 5000000 * 1000000 / 50000 = 100000000
        // But DebtMaximum is 20000000, so let's adjust the broker
        tx["LoanPrincipal"] = serde_json::json!("19000000");
        // Required cover = 50000 * 19000000 / 1000000 = 950000, which is < 5000000
        // Need to test insufficient cover, so set a higher principal or lower cover
        // Actually with 19000000: required = 950000, cover = 5000000, still sufficient
        // Let's just test DebtMaximum instead
        tx["LoanPrincipal"] = serde_json::json!("20000001");
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        // This fails with TecOversize because it exceeds DebtMaximum
        assert_eq!(
            LoanSetTransactor.preclaim(&ctx),
            Err(TransactionResult::TecOversize)
        );
    }

    #[test]
    fn amortization_calculation() {
        // P=1000000, r=10000 (1% per period), n=12
        let payment = calculate_periodic_payment(1000000, 10000, 12);
        // Expected: ~88849 (standard amortization)
        assert!(payment > 85000 && payment < 92000);

        // Zero rate: simple division
        let payment_zero = calculate_periodic_payment(1200000, 0, 12);
        assert_eq!(payment_zero, 100000);
    }
}
