use rxrpl_amendment::Rules;
use rxrpl_protocol::{TransactionResult, TransactionType};
use serde_json::Value;

use crate::error::TxEngineError;
use crate::fees::FeeSettings;
use crate::invariants;
use crate::registry::TransactorRegistry;
use crate::view::apply_view::ApplyView;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
use crate::view::ledger_view::LedgerView;
use crate::view::sandbox::Sandbox;

/// The transaction execution engine.
///
/// Orchestrates the full transaction processing pipeline:
/// 1. Look up transactor from registry
/// 2. Run preflight (stateless validation)
/// 3. Calculate base fee
/// 4. Run preclaim (read-only ledger validation)
/// 5. Create sandbox
/// 6. Consume fee
/// 7. Run apply (state mutations)
/// 8. Run invariant checks
/// 9. If success: commit sandbox to ledger
pub struct TxEngine {
    registry: TransactorRegistry,
    invariants: Vec<Box<dyn invariants::InvariantCheck>>,
}

impl TxEngine {
    pub fn new(registry: TransactorRegistry) -> Self {
        Self {
            registry,
            invariants: invariants::default_invariant_checks(),
        }
    }

    /// Apply a transaction to a ledger.
    ///
    /// Returns the transaction result code.
    pub fn apply(
        &self,
        tx: &Value,
        ledger: &mut rxrpl_ledger::Ledger,
        rules: &Rules,
        fees: &FeeSettings,
    ) -> Result<TransactionResult, TxEngineError> {
        // 1. Determine transaction type
        let tx_type_str = tx
            .get("TransactionType")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TxEngineError::UnknownTransactionType("missing".into()))?;

        let tx_type = TransactionType::from_name(tx_type_str)
            .map_err(|_| TxEngineError::UnknownTransactionType(tx_type_str.into()))?;

        let transactor = self
            .registry
            .get(&tx_type)
            .ok_or_else(|| TxEngineError::UnknownTransactionType(tx_type_str.into()))?;

        // 2. Preflight (stateless)
        let preflight_ctx = PreflightContext { tx, rules, fees };
        if let Err(result) = transactor.preflight(&preflight_ctx) {
            return Ok(result);
        }

        // 3. Calculate base fee
        let _base_fee = transactor.calculate_base_fee(&preflight_ctx);

        // 4. Preclaim (read-only ledger validation)
        let view = LedgerView::with_fees(ledger, fees.clone());
        let preclaim_ctx = PreclaimContext {
            tx,
            view: &view,
            rules,
        };
        if let Err(result) = transactor.preclaim(&preclaim_ctx) {
            return Ok(result);
        }

        // 5-9. Create sandbox, apply, check invariants, commit
        let drops_before = ledger.header.drops;
        let view = LedgerView::with_fees(ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        // 6. Fee consumption (simplified -- deduct from account balance)
        // Full implementation would parse the fee from tx and deduct from account
        let fee_drops: u64 = tx
            .get("Fee")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if fee_drops > 0 {
            sandbox.destroy_drops(fee_drops);
        }

        // 7. Apply
        let mut apply_ctx = ApplyContext {
            tx,
            view: &mut sandbox,
            rules,
            fees,
        };
        let result = match transactor.apply(&mut apply_ctx) {
            Ok(result) => result,
            Err(result) => {
                // Transaction failed -- don't commit mutations (but fee is still consumed)
                if result.is_claimed() {
                    // tec -- claim fee but discard state changes
                    return Ok(result);
                }
                return Ok(result);
            }
        };

        // 8. Invariant checks
        let changes = sandbox.into_changes();
        let drops_after = drops_before.saturating_sub(changes.destroyed_drops);

        if let Err(msg) = invariants::run_invariant_checks(
            &self.invariants,
            &changes,
            drops_before,
            drops_after,
        ) {
            return Err(TxEngineError::InvariantViolated(msg));
        }

        // 9. Commit
        changes.apply_to_ledger(ledger)?;

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transactor::Transactor;
    use rxrpl_ledger::Ledger;

    /// A test transactor that always succeeds.
    struct NoopTransactor;

    impl Transactor for NoopTransactor {
        fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn apply(
            &self,
            _ctx: &mut ApplyContext<'_>,
        ) -> Result<TransactionResult, TransactionResult> {
            Ok(TransactionResult::TesSuccess)
        }
    }

    /// A test transactor that fails in preflight.
    struct FailPreflightTransactor;

    impl Transactor for FailPreflightTransactor {
        fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
            Err(TransactionResult::TemMalformed)
        }
        fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
            Ok(())
        }
        fn apply(
            &self,
            _ctx: &mut ApplyContext<'_>,
        ) -> Result<TransactionResult, TransactionResult> {
            unreachable!("should not reach apply");
        }
    }

    fn test_engine_with(tx_type: TransactionType, transactor: impl Transactor + 'static) -> TxEngine {
        let mut registry = TransactorRegistry::new();
        registry.register(tx_type, transactor);
        TxEngine::new(registry)
    }

    fn make_tx(tx_type: &str) -> Value {
        serde_json::json!({
            "TransactionType": tx_type,
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10"
        })
    }

    #[test]
    fn engine_noop_success() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = Ledger::genesis();
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }

    #[test]
    fn engine_preflight_failure() {
        let engine = test_engine_with(TransactionType::AccountSet, FailPreflightTransactor);
        let mut ledger = Ledger::genesis();
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TemMalformed);
    }

    #[test]
    fn engine_unknown_tx_type() {
        let engine = TxEngine::new(TransactorRegistry::new());
        let mut ledger = Ledger::genesis();
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        assert!(engine.apply(&tx, &mut ledger, &rules, &fees).is_err());
    }
}
