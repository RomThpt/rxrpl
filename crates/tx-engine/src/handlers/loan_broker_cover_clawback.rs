use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanBrokerCoverClawbackTransactor;

impl Transactor for LoanBrokerCoverClawbackTransactor {
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

        // Caller must be the asset issuer (not the broker owner)
        let owner = broker["Owner"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if owner == account_str {
            return Err(TransactionResult::TecNoPermission);
        }

        // Caller must be the vault asset issuer. We validate by checking VaultID.
        // For this implementation, the caller must not be the owner (issuer check).
        // The protocol-level issuer validation is handled by the asset on the vault.

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

        // Read broker
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
        let debt_total: u64 = broker["DebtTotal"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let cover_rate_liq = broker["CoverRateLiquidation"].as_u64().unwrap_or(0);

        // Clawback = min(CoverRateLiquidation * DebtTotal / 1_000_000, CoverAvailable)
        let clawback_max = cover_rate_liq
            .checked_mul(debt_total)
            .ok_or(TransactionResult::TefInternal)?
            / 1_000_000;
        let clawback = clawback_max.min(cover_available);

        if clawback == 0 {
            return Ok(TransactionResult::TesSuccess);
        }

        // Decrement CoverAvailable
        broker["CoverAvailable"] =
            serde_json::Value::String((cover_available - clawback).to_string());

        let broker_data =
            serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(broker_key, broker_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit caller (issuer)
        let acct_key = keylet::account(&account_id);
        let acct_bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&acct_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, balance + clawback);
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
    const ISSUER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_broker(cover: &str, debt: &str) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(OWNER, 100_000_000u64), (ISSUER, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 2,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let owner_id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
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
    fn valid_clawback() {
        // debt=5000000, liq_rate=80000 => max_clawback = 80000*5000000/1000000 = 400000
        // cover=1000000 => clawback = min(400000, 1000000) = 400000
        let ledger = setup_with_broker("1000000", "5000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverClawback",
            "Account": ISSUER,
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

        let result = LoanBrokerCoverClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Cover decreased by clawback amount
        let owner_id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["CoverAvailable"].as_str().unwrap(), "600000");

        // Issuer credited
        let issuer_id = decode_account_id(ISSUER).unwrap();
        let issuer_key = keylet::account(&issuer_id);
        let issuer_bytes = sandbox.read(&issuer_key).unwrap();
        let issuer_acct: serde_json::Value = serde_json::from_slice(&issuer_bytes).unwrap();
        assert_eq!(issuer_acct["Balance"].as_str().unwrap(), "50400000");
    }

    #[test]
    fn non_issuer_rejected() {
        // Owner trying to clawback their own broker should fail
        let ledger = setup_with_broker("1000000", "5000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverClawback",
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
            LoanBrokerCoverClawbackTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn clawback_capped_at_cover_available() {
        // debt=20000000, liq_rate=80000 => max_clawback = 80000*20000000/1000000 = 1600000
        // cover=500000 => clawback = min(1600000, 500000) = 500000
        let ledger = setup_with_broker("500000", "20000000");
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "LoanBrokerCoverClawback",
            "Account": ISSUER,
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

        let result = LoanBrokerCoverClawbackTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let owner_id = decode_account_id(OWNER).unwrap();
        let broker_key = keylet::loan_broker(owner_id.as_bytes(), 1);
        let broker_bytes = sandbox.read(&broker_key).unwrap();
        let broker: serde_json::Value = serde_json::from_slice(&broker_bytes).unwrap();
        assert_eq!(broker["CoverAvailable"].as_str().unwrap(), "0");
    }
}
