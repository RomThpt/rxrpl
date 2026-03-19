use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// EnableAmendment pseudo-transaction handler.
///
/// Manages the amendment process:
/// - `tfGotMajority` (0x00010000): add amendment to Majorities
/// - `tfLostMajority` (0x00020000): remove amendment from Majorities
/// - flags == 0: activate amendment (add to Amendments, remove from Majorities)
pub struct EnableAmendmentTransactor;

const TF_GOT_MAJORITY: u32 = 0x00010000;
const TF_LOST_MAJORITY: u32 = 0x00020000;

impl Transactor for EnableAmendmentTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amendment hash must be present (64-char hex)
        let amendment =
            helpers::get_str_field(ctx.tx, "Amendment").ok_or(TransactionResult::TemMalformed)?;
        if amendment.len() != 64 || !amendment.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(TransactionResult::TemMalformed);
        }

        // Flags must be 0, tfGotMajority, or tfLostMajority
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);
        if flags != 0 && flags != TF_GOT_MAJORITY && flags != TF_LOST_MAJORITY {
            return Err(TransactionResult::TemInvalidFlag);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);

        // If activating (flags==0), verify not already active
        if flags == 0 {
            let amendment_hash = helpers::get_str_field(ctx.tx, "Amendment").unwrap();
            let amendments_key = keylet::amendments();
            if let Some(data) = ctx.view.read(&amendments_key) {
                if let Ok(obj) = serde_json::from_slice::<Value>(&data) {
                    if let Some(list) = obj.get("Amendments").and_then(|v| v.as_array()) {
                        if list.iter().any(|v| v.as_str() == Some(amendment_hash)) {
                            return Err(TransactionResult::TefPastSeq);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let amendment_hash =
            helpers::get_str_field(ctx.tx, "Amendment").ok_or(TransactionResult::TefInternal)?;
        let flags = helpers::get_u32_field(ctx.tx, "Flags").unwrap_or(0);

        let amendments_key = keylet::amendments();
        let (mut obj, exists) = if let Some(data) = ctx.view.read(&amendments_key) {
            let obj: Value =
                serde_json::from_slice(&data).map_err(|_| TransactionResult::TefInternal)?;
            (obj, true)
        } else {
            (
                serde_json::json!({
                    "LedgerEntryType": "Amendments",
                    "Amendments": [],
                    "Majorities": [],
                }),
                false,
            )
        };

        match flags {
            TF_GOT_MAJORITY => {
                // Add to Majorities
                let majorities = obj
                    .get_mut("Majorities")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(TransactionResult::TefInternal)?;

                // Don't add duplicate
                if !majorities
                    .iter()
                    .any(|m| m.get("Amendment").and_then(|v| v.as_str()) == Some(amendment_hash))
                {
                    majorities.push(serde_json::json!({
                        "Amendment": amendment_hash,
                        "CloseTime": ctx.tx.get("CloseTime").cloned().unwrap_or(Value::from(0)),
                    }));
                }
            }
            TF_LOST_MAJORITY => {
                // Remove from Majorities
                if let Some(majorities) = obj.get_mut("Majorities").and_then(|v| v.as_array_mut()) {
                    majorities.retain(|m| {
                        m.get("Amendment").and_then(|v| v.as_str()) != Some(amendment_hash)
                    });
                }
            }
            0 => {
                // Activate: add to Amendments list
                let amendments = obj
                    .get_mut("Amendments")
                    .and_then(|v| v.as_array_mut())
                    .ok_or(TransactionResult::TefInternal)?;
                amendments.push(Value::String(amendment_hash.to_string()));

                // Remove from Majorities
                if let Some(majorities) = obj.get_mut("Majorities").and_then(|v| v.as_array_mut()) {
                    majorities.retain(|m| {
                        m.get("Amendment").and_then(|v| v.as_str()) != Some(amendment_hash)
                    });
                }
            }
            _ => return Err(TransactionResult::TefInternal),
        }

        let data = serde_json::to_vec(&obj).map_err(|_| TransactionResult::TefInternal)?;
        if exists {
            ctx.view
                .update(amendments_key, data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            ctx.view
                .insert(amendments_key, data)
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

    const AMENDMENT_HASH: &str = "42426C4D4F1009EE67080A9B7965B44656D7714D104A72F9B4369F97ABF044EE";

    fn make_enable_amendment_tx(amendment: &str, flags: u32) -> Value {
        serde_json::json!({
            "TransactionType": "EnableAmendment",
            "Amendment": amendment,
            "Flags": flags,
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_valid_got_majority() {
        let tx = make_enable_amendment_tx(AMENDMENT_HASH, TF_GOT_MAJORITY);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(EnableAmendmentTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn preflight_missing_amendment() {
        let tx = serde_json::json!({
            "TransactionType": "EnableAmendment",
            "Flags": 0,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            EnableAmendmentTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_invalid_amendment_length() {
        let tx = make_enable_amendment_tx("ABCD", 0);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            EnableAmendmentTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_invalid_flags() {
        let tx = make_enable_amendment_tx(AMENDMENT_HASH, 0x00030000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            EnableAmendmentTransactor.preflight(&ctx),
            Err(TransactionResult::TemInvalidFlag)
        );
    }

    // -- apply tests --

    #[test]
    fn apply_got_majority_adds_to_majorities() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_enable_amendment_tx(AMENDMENT_HASH, TF_GOT_MAJORITY);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = EnableAmendmentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify amendment in Majorities
        let key = keylet::amendments();
        let data = sandbox.read(&key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        let majorities = obj["Majorities"].as_array().unwrap();
        assert_eq!(majorities.len(), 1);
        assert_eq!(majorities[0]["Amendment"].as_str().unwrap(), AMENDMENT_HASH);
    }

    #[test]
    fn apply_lost_majority_removes_from_majorities() {
        let mut ledger = Ledger::genesis();

        // Pre-populate Amendments object with a majority entry
        let amendments_key = keylet::amendments();
        let amendments_obj = serde_json::json!({
            "LedgerEntryType": "Amendments",
            "Amendments": [],
            "Majorities": [{ "Amendment": AMENDMENT_HASH, "CloseTime": 0 }],
        });
        ledger
            .put_state(amendments_key, serde_json::to_vec(&amendments_obj).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_enable_amendment_tx(AMENDMENT_HASH, TF_LOST_MAJORITY);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = EnableAmendmentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let data = sandbox.read(&amendments_key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        let majorities = obj["Majorities"].as_array().unwrap();
        assert!(majorities.is_empty());
    }

    #[test]
    fn apply_activate_adds_to_amendments() {
        let mut ledger = Ledger::genesis();

        let amendments_key = keylet::amendments();
        let amendments_obj = serde_json::json!({
            "LedgerEntryType": "Amendments",
            "Amendments": [],
            "Majorities": [{ "Amendment": AMENDMENT_HASH, "CloseTime": 0 }],
        });
        ledger
            .put_state(amendments_key, serde_json::to_vec(&amendments_obj).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_enable_amendment_tx(AMENDMENT_HASH, 0);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = EnableAmendmentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let data = sandbox.read(&amendments_key).unwrap();
        let obj: Value = serde_json::from_slice(&data).unwrap();
        let amendments = obj["Amendments"].as_array().unwrap();
        assert_eq!(amendments.len(), 1);
        assert_eq!(amendments[0].as_str().unwrap(), AMENDMENT_HASH);
        // Majorities should be cleared
        let majorities = obj["Majorities"].as_array().unwrap();
        assert!(majorities.is_empty());
    }
}
