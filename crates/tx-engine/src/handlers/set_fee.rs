use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// SetFee pseudo-transaction handler.
///
/// Updates the FeeSettings ledger object with new fee parameters.
pub struct SetFeeTransactor;

impl Transactor for SetFeeTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // At least one fee field must be present
        let has_base = helpers::get_str_field(ctx.tx, "BaseFee").is_some()
            || helpers::get_str_field(ctx.tx, "BaseFeeDrops").is_some();
        let has_reserve = helpers::get_str_field(ctx.tx, "ReserveBase").is_some()
            || helpers::get_str_field(ctx.tx, "ReserveBaseDrops").is_some();
        let has_increment = helpers::get_str_field(ctx.tx, "ReserveIncrement").is_some()
            || helpers::get_str_field(ctx.tx, "ReserveIncrementDrops").is_some();

        if !has_base && !has_reserve && !has_increment {
            return Err(TransactionResult::TemMalformed);
        }

        Ok(())
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let fee_key = keylet::fee_settings();
        let (mut obj, exists) = if let Some(data) = ctx.view.read(&fee_key) {
            let obj: Value =
                serde_json::from_slice(&data).map_err(|_| TransactionResult::TefInternal)?;
            (obj, true)
        } else {
            (
                serde_json::json!({
                    "LedgerEntryType": "FeeSettings",
                }),
                false,
            )
        };

        // Update BaseFee (prefer *Drops variant)
        if let Some(v) = helpers::get_str_field(ctx.tx, "BaseFeeDrops") {
            obj["BaseFeeDrops"] = Value::String(v.to_string());
        } else if let Some(v) = helpers::get_str_field(ctx.tx, "BaseFee") {
            obj["BaseFee"] = Value::String(v.to_string());
        }

        // Update ReserveBase
        if let Some(v) = helpers::get_str_field(ctx.tx, "ReserveBaseDrops") {
            obj["ReserveBaseDrops"] = Value::String(v.to_string());
        } else if let Some(v) = helpers::get_str_field(ctx.tx, "ReserveBase") {
            obj["ReserveBase"] = Value::String(v.to_string());
        }

        // Update ReserveIncrement
        if let Some(v) = helpers::get_str_field(ctx.tx, "ReserveIncrementDrops") {
            obj["ReserveIncrementDrops"] = Value::String(v.to_string());
        } else if let Some(v) = helpers::get_str_field(ctx.tx, "ReserveIncrement") {
            obj["ReserveIncrement"] = Value::String(v.to_string());
        }

        let data = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TefInternal)?;
        if exists {
            ctx.view
                .update(fee_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            ctx.view
                .insert(fee_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    #[test]
    fn preflight_missing_all_fields() {
        let tx = serde_json::json!({
            "TransactionType": "SetFee",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetFeeTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_valid_with_base_fee() {
        let tx = serde_json::json!({
            "TransactionType": "SetFee",
            "BaseFee": "10",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(SetFeeTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn apply_creates_fee_settings() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "SetFee",
            "BaseFeeDrops": "15",
            "ReserveBaseDrops": "20000000",
            "ReserveIncrementDrops": "5000000",
        });
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = SetFeeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let key = keylet::fee_settings();
        let data = sandbox.read(&key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(obj["BaseFeeDrops"].as_str().unwrap(), "15");
        assert_eq!(obj["ReserveBaseDrops"].as_str().unwrap(), "20000000");
        assert_eq!(obj["ReserveIncrementDrops"].as_str().unwrap(), "5000000");
    }

    #[test]
    fn apply_updates_existing_fee_settings() {
        let mut ledger = Ledger::genesis();
        let fee_key = keylet::fee_settings();
        let existing = serde_json::json!({
            "LedgerEntryType": "FeeSettings",
            "BaseFeeDrops": "10",
            "ReserveBaseDrops": "10000000",
        });
        ledger
            .put_state(fee_key, serde_json::to_vec(&existing).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = serde_json::json!({
            "TransactionType": "SetFee",
            "BaseFeeDrops": "20",
        });
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = SetFeeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let data = sandbox.read(&fee_key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(obj["BaseFeeDrops"].as_str().unwrap(), "20");
        // Original field preserved
        assert_eq!(obj["ReserveBaseDrops"].as_str().unwrap(), "10000000");
    }
}
