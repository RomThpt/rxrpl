use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// SetHook transaction handler (stub).
///
/// WASM hooks are not yet supported. Returns `TemDisabled` at preflight.
pub struct SetHookTransactor;

impl Transactor for SetHookTransactor {
    fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        Err(TransactionResult::TemDisabled)
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        Ok(())
    }

    fn apply(&self, _ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use rxrpl_amendment::Rules;

    #[test]
    fn preflight_returns_disabled() {
        let tx = serde_json::json!({
            "TransactionType": "SetHook",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            SetHookTransactor.preflight(&ctx),
            Err(TransactionResult::TemDisabled)
        );
    }
}
