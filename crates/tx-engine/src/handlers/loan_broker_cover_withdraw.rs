use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerCoverWithdrawTransactor;

impl Transactor for LoanBrokerCoverWithdrawTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_str_field(ctx.tx, "LoanBrokerOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;

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

        // Caller must be Owner
        let owner = broker["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner != account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;

        let cover_available: u64 = broker["CoverAvailable"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Amount must not exceed CoverAvailable
        if amount > cover_available {
            return Err(TransactionResult::TecInsufficientFunds);
        }

        // Cover minimum must be maintained after withdrawal:
        // (CoverAvailable - Amount) >= CoverRateMinimum * DebtTotal / 1_000_000
        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let cover_rate_min = broker["CoverRateMinimum"].as_u64().unwrap_or(0);

        if debt_total > 0 {
            let required_cover = cover_rate_min
                .checked_mul(debt_total)
                .ok_or(TransactionResult::TefInternal)?
                / 1_000_000;
            if cover_available - amount < required_cover {
                return Err(TransactionResult::TecInsufficientReserve);
            }
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
        let amount =
            helpers::get_u64_str_field(ctx.tx, "Amount").ok_or(TransactionResult::TemBadAmount)?;

        let broker_owner_id = decode_account_id(&broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);

        // Decrement CoverAvailable
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

        broker["CoverAvailable"] =
            serde_json::Value::String((cover_available - amount).to_string());

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit caller
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
    use crate::transactor::{ApplyContext, PreclaimContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const OWNER: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    fn setup_with_broker(cover: &str, debt: &str) -> Ledger {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(OWNER).unwrap();
        let key = keylet::account(&id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": OWNER,
            "Balance": "100000000",
            "Sequence": 2,
            "OwnerCount": 2,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let broker_key = keylet::loan_broker(id.as_bytes(), 1);
        let broker = serde_json::json!({
            "LedgerEntryType": "LoanBroker",
            "Owner": OWNER,
            "Account": OWNER,
            "VaultID": format!("{}:1", OWNER),
            "LoanSequence": 1,
            "OwnerCount": 0,
            "DebtTotal": debt,
            "DebtMaximum": "10000000",
            "CoverAvailable": cover,
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

    #[test]
    fn valid_withdraw() {
        let ledger = setup_with_broker("5000000", "0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverWithdraw",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
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

        let result = LoanBrokerCoverWithdrawTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["CoverAvailable"].as_str().unwrap(), "2000000");

        let acct_key = keylet::account(&id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "103000000");
    }

    #[test]
    fn cover_minimum_violation() {
        // debt=10000000, cover_rate_min=50000 => required_cover = 50000*10000000/1000000 = 500000
        // cover=600000, withdraw 200000 => remaining=400000 < 500000
        let ledger = setup_with_broker("600000", "10000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverWithdraw",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Amount": "200000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanBrokerCoverWithdrawTransactor.preclaim(&ctx),
            Err(TransactionResult::TecInsufficientReserve)
        );
    }

    #[test]
    fn withdraw_exceeds_cover() {
        let ledger = setup_with_broker("1000000", "0");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverWithdraw",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Amount": "2000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanBrokerCoverWithdrawTransactor.preclaim(&ctx),
            Err(TransactionResult::TecInsufficientFunds)
        );
    }
}
