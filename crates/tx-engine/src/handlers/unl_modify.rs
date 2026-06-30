use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// UNLModify pseudo-transaction handler.
///
/// Mirrors rippled's `Change::applyUNLModify`: the transaction only records
/// the *pending* intent on the `NegativeUNL` singleton object — it does NOT
/// touch `DisabledValidators`:
/// - `UNLModifyDisabling == 1`: set `ValidatorToDisable` to the public key.
/// - `UNLModifyDisabling == 0`: set `ValidatorToReEnable` to the public key.
///
/// The actual move into / out of `DisabledValidators` (with the
/// `DisabledValidator` STObject wrapper and `FirstLedgerSequence` stamp) is
/// DEFERRED to the build of the next flag ledger — see
/// `rxrpl_node::Node::update_negative_unl`, which mirrors
/// `Ledger::updateNegativeUNL`.
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
        // Load the NegativeUNL singleton, or create it fresh. A freshly
        // created ledger entry carries only the required common fields
        // (`LedgerEntryType` + `Flags`); rippled does not serialize an empty
        // `DisabledValidators` array, so we omit it here for byte parity.
        let (mut obj, exists) = if let Some(data) = ctx.view.read(&nunl_key) {
            let obj: Value =
                serde_json::from_slice(&data).map_err(|_| TransactionResult::TefInternal)?;
            (obj, true)
        } else {
            (
                serde_json::json!({
                    "LedgerEntryType": "NegativeUNL",
                    "Flags": 0,
                }),
                false,
            )
        };

        // Record ONLY the pending field. `DisabledValidators` is intentionally
        // left untouched here; the deferred flag-ledger move
        // (`Node::update_negative_unl`) is the sole writer of that array.
        if disabling == 1 {
            obj["ValidatorToDisable"] = Value::String(validator.to_string());
        } else {
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
    fn apply_disable_sets_pending_only() {
        // Deferred model: disabling records `ValidatorToDisable` and must
        // NOT touch `DisabledValidators` (that move is deferred to the next
        // flag-ledger build).
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
        assert_eq!(obj["ValidatorToDisable"].as_str().unwrap(), VALIDATOR_KEY);
        // DisabledValidators is left absent until the deferred move.
        assert!(
            obj.get("DisabledValidators").is_none(),
            "handler must not populate DisabledValidators: {obj}"
        );
        assert!(obj.get("ValidatorToReEnable").is_none());
    }

    #[test]
    fn apply_reenable_sets_pending_only() {
        // Deferred model: re-enabling records `ValidatorToReEnable` and must
        // leave the existing `DisabledValidators` array untouched (the entry
        // is removed later by the deferred flag-ledger move).
        let mut ledger = Ledger::genesis();

        // Pre-populate with a disabled validator using the byte-exact
        // `DisabledValidator` wrapper shape.
        let nunl_key = keylet::negative_unl();
        let nunl_obj = serde_json::json!({
            "LedgerEntryType": "NegativeUNL",
            "Flags": 0,
            "DisabledValidators": [{
                "DisabledValidator": {
                    "PublicKey": VALIDATOR_KEY,
                    "FirstLedgerSequence": 256u32,
                }
            }],
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
        assert_eq!(obj["ValidatorToReEnable"].as_str().unwrap(), VALIDATOR_KEY);
        // The disabled entry is still present — the handler does not remove it.
        let disabled = obj["DisabledValidators"].as_array().unwrap();
        assert_eq!(disabled.len(), 1);
        assert_eq!(
            disabled[0]["DisabledValidator"]["PublicKey"]
                .as_str()
                .unwrap(),
            VALIDATOR_KEY
        );
        assert!(obj.get("ValidatorToDisable").is_none());
    }
}
