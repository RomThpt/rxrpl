use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerDeleteTransactor;

impl Transactor for LoanBrokerDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_str_field(ctx.tx, "LoanBrokerOwner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "LoanBrokerSequence")
            .ok_or(TransactionResult::TemMalformed)?;
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

        // OwnerCount must be 0 (no active loans)
        let owner_count = broker["OwnerCount"].as_u64().unwrap_or(0);
        if owner_count != 0 {
            return Err(TransactionResult::TecHasObligations);
        }

        // DebtTotal must be "0"
        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if debt_total != 0 {
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

        let broker_owner_id = decode_account_id(&broker_owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let broker_key = keylet::loan_broker(broker_owner_id.as_bytes(), broker_seq);

        // Read broker to get CoverAvailable
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let cover_available: u64 = broker["CoverAvailable"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Delete broker entry
        ctx.view
            .erase(&broker_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Return CoverAvailable to owner and adjust owner count
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        if cover_available > 0 {
            let balance = helpers::get_balance(&account);
            helpers::set_balance(&mut account, balance + cover_available);
        }

        helpers::adjust_owner_count(&mut account, -2);

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

    fn setup_with_broker(owner_count: u64, debt_total: &str, cover: &str) -> Ledger {
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
            "OwnerCount": owner_count,
            "DebtTotal": debt_total,
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
    fn active_loans_prevent_delete() {
        let ledger = setup_with_broker(1, "5000000", "3000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerDelete",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanBrokerDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }

    #[test]
    fn valid_delete() {
        let ledger = setup_with_broker(0, "0", "2000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerDelete",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = LoanBrokerDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Broker entry should be gone
        let id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(id.as_bytes(), 1);
        assert!(sandbox.read(&broker_key).is_none());

        // Cover returned to owner
        let acct_key = keylet::account(&id);
        let acct_bytes = sandbox.read(&acct_key).unwrap();
        let acct: serde_json::Value = serde_json::from_slice(&acct_bytes).unwrap();
        assert_eq!(acct["Balance"].as_str().unwrap(), "102000000");
        assert_eq!(acct["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn outstanding_debt_prevents_delete() {
        let ledger = setup_with_broker(0, "1000000", "500000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerDelete",
            "Account": OWNER,
            "LoanBrokerOwner": OWNER,
            "LoanBrokerSequence": 1,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            LoanBrokerDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }
}
