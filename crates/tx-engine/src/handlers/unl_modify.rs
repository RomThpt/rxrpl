use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// UNLModify pseudo-transaction handler.
///
/// Modifies the Negative UNL by adding or removing validators.
/// - `UNLModifyDisabling == 1`: disable a validator (add to DisabledValidators)
/// - `UNLModifyDisabling == 0`: re-enable a validator (remove from DisabledValidators)
pub struct UNLModifyTransactor;

impl Transactor for UNLModifyTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // UNLModifyDisabling must be 0 or 1
        let disabling = helpers::get_u32_field(ctx.tx, "UNLModifyDisabling")
            .ok_or(TransactionResult::TemMalformed)?;
        if disabling > 1 {
            return Err(TransactionResult::TemMalformed);
        }

        // UNLModifyValidator must be present
        helpers::get_str_field(ctx.tx, "UNLModifyValidator")
            .ok_or(TransactionResult::TemMalformed)?;

        Ok(())
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let disabling = helpers::get_u32_field(ctx.tx, "UNLModifyDisabling").unwrap_or(0);
        let validator = helpers::get_str_field(ctx.tx, "UNLModifyValidator")
            .ok_or(TransactionResult::TefInternal)?;

        let nunl_key = keylet::negative_unl();
        let (mut obj, exists) = if let Some(data) = ctx.view.read(&nunl_key) {
            let obj: Value =
                serde_json::from_slice(&data).map_err(|_| TransactionResult::TefInternal)?;
            (obj, true)
        } else {
            (
                serde_json::json!({
                    "LedgerEntryType": "NegativeUNL",
                    "DisabledValidators": [],
                }),
                false,
            )
        };

        if disabling == 1 {
            // Add to DisabledValidators
            let disabled = obj
                .get_mut("DisabledValidators")
                .and_then(|v| v.as_array_mut())
                .ok_or(TransactionResult::TefInternal)?;

            // Don't add duplicate
            if !disabled
                .iter()
                .any(|d| d.get("PublicKey").and_then(|v| v.as_str()) == Some(validator))
            {
                disabled.push(serde_json::json!({
                    "PublicKey": validator,
                    "LedgerSequence": ctx.tx.get("LedgerSequence").cloned().unwrap_or(Value::from(0)),
                }));
            }

            obj["ValidatorToDisable"] = Value::String(validator.to_string());
        } else {
            // Remove from DisabledValidators
            if let Some(disabled) = obj
                .get_mut("DisabledValidators")
                .and_then(|v| v.as_array_mut())
            {
                disabled.retain(|d| d.get("PublicKey").and_then(|v| v.as_str()) != Some(validator));
            }

            obj["ValidatorToReEnable"] = Value::String(validator.to_string());
        }

        let data = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TefInternal)?;
        if exists {
            ctx.view
                .update(nunl_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            ctx.view
                .insert(nunl_key, data)
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

    const VALIDATOR_KEY: &str =
        "ED6629D456285AE3613B285F65BBFF168D695BA3921F309949AFCD2CA7AFEC16FE";

    fn make_unl_modify_tx(disabling: u32, validator: &str) -> Value {
        serde_json::json!({
            "TransactionType": "UNLModify",
            "UNLModifyDisabling": disabling,
            "UNLModifyValidator": validator,
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_missing_disabling() {
        let tx = serde_json::json!({
            "TransactionType": "UNLModify",
            "UNLModifyValidator": VALIDATOR_KEY,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            UNLModifyTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_invalid_disabling() {
        let tx = make_unl_modify_tx(2, VALIDATOR_KEY);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            UNLModifyTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_validator() {
        let tx = serde_json::json!({
            "TransactionType": "UNLModify",
            "UNLModifyDisabling": 1,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            UNLModifyTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_valid() {
        let tx = make_unl_modify_tx(1, VALIDATOR_KEY);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(UNLModifyTransactor.preflight(&ctx).is_ok());
    }

    // -- apply tests --

    #[test]
    fn apply_disable_validator() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_unl_modify_tx(1, VALIDATOR_KEY);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = UNLModifyTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let key = keylet::negative_unl();
        let data = sandbox.read(&key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        let disabled = obj["DisabledValidators"].as_array().unwrap();
        assert_eq!(disabled.len(), 1);
        assert_eq!(disabled[0]["PublicKey"].as_str().unwrap(), VALIDATOR_KEY);
        assert_eq!(obj["ValidatorToDisable"].as_str().unwrap(), VALIDATOR_KEY);
    }

    #[test]
    fn apply_reenable_validator() {
        let mut ledger = Ledger::genesis();

        // Pre-populate with disabled validator
        let nunl_key = keylet::negative_unl();
        let nunl_obj = serde_json::json!({
            "LedgerEntryType": "NegativeUNL",
            "DisabledValidators": [{ "PublicKey": VALIDATOR_KEY, "LedgerSequence": 0 }],
        });
        ledger
            .put_state(nunl_key, serde_json::to_vec(&nunl_obj).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_unl_modify_tx(0, VALIDATOR_KEY);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = UNLModifyTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let data = sandbox.read(&nunl_key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        let disabled = obj["DisabledValidators"].as_array().unwrap();
        assert!(disabled.is_empty());
        assert_eq!(obj["ValidatorToReEnable"].as_str().unwrap(), VALIDATOR_KEY);
    }
}
