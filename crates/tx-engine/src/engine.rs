use rxrpl_amendment::Rules;
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult, TransactionType};
use serde_json::Value;

use crate::error::TxEngineError;
use crate::fees::FeeSettings;
use crate::helpers;
use crate::invariants;
use crate::registry::TransactorRegistry;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
use crate::view::apply_view::ApplyView;
use crate::view::ledger_view::LedgerView;
use crate::view::read_view::ReadView;
use crate::view::sandbox::Sandbox;

/// The transaction execution engine.
///
/// Orchestrates the full transaction processing pipeline:
/// 1. Look up transactor from registry
/// 2. Run preflight (stateless validation)
/// 3. Verify signature
/// 4. Calculate base fee
/// 5. Run preclaim (read-only ledger validation)
/// 6. Create sandbox, deduct fee from sender account
/// 7. Run apply in nested (child) sandbox
/// 8. Run invariant checks
/// 9. Commit sandbox to ledger + record tx in tx_map
pub struct TxEngine {
    registry: TransactorRegistry,
    invariants: Vec<Box<dyn invariants::InvariantCheck>>,
    skip_signature_verification: bool,
}

impl TxEngine {
    pub fn new(registry: TransactorRegistry) -> Self {
        Self {
            registry,
            invariants: invariants::default_invariant_checks(),
            skip_signature_verification: false,
        }
    }

    /// Create an engine that skips signature verification.
    ///
    /// Useful for unit tests that use unsigned transactions.
    pub fn new_without_sig_check(registry: TransactorRegistry) -> Self {
        Self {
            registry,
            invariants: invariants::default_invariant_checks(),
            skip_signature_verification: true,
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

        // 3. Verify signature
        if !self.skip_signature_verification {
            if let Err(_e) = rxrpl_protocol::tx::verify_signature(tx) {
                return Ok(TransactionResult::TefBadSignature);
            }
        }

        // 4. Calculate base fee
        let _base_fee = transactor.calculate_base_fee(&preflight_ctx);

        // 5. Preclaim (read-only ledger validation)
        let view = LedgerView::with_fees(ledger, fees.clone());
        let preclaim_ctx = PreclaimContext {
            tx,
            view: &view,
            rules,
        };
        if let Err(result) = transactor.preclaim(&preclaim_ctx) {
            return Ok(result);
        }

        // 6. Create sandbox and deduct fee from sender account
        let drops_before = ledger.header.drops;
        let view = LedgerView::with_fees(ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);

        let fee_drops = helpers::get_fee(tx);
        if fee_drops > 0 {
            let account_str = helpers::get_account(tx)
                .map_err(TxEngineError::TransactionFailed)?;
            let account_id = decode_account_id(account_str)
                .map_err(|_| TxEngineError::TransactionFailed(TransactionResult::TemInvalidAccountId))?;
            let account_key = keylet::account(&account_id);

            let account_bytes = sandbox
                .read(&account_key)
                .ok_or(TxEngineError::TransactionFailed(TransactionResult::TerNoAccount))?;
            let mut account_obj: Value = serde_json::from_slice(&account_bytes)
                .map_err(|_| TxEngineError::TransactionFailed(TransactionResult::TefInternal))?;

            let balance = helpers::get_balance(&account_obj);
            if balance < fee_drops {
                return Ok(TransactionResult::TerInsufFee);
            }
            helpers::set_balance(&mut account_obj, balance - fee_drops);

            let updated = serde_json::to_vec(&account_obj)
                .map_err(|_| TxEngineError::TransactionFailed(TransactionResult::TefInternal))?;
            sandbox
                .update(account_key, updated)
                .map_err(|_| TxEngineError::TransactionFailed(TransactionResult::TefInternal))?;

            sandbox.destroy_drops(fee_drops);
        }

        // 7. Apply in child sandbox
        let mut child = sandbox.child();
        let mut apply_ctx = ApplyContext {
            tx,
            view: &mut child,
            rules,
            fees,
        };

        let handler_result = transactor.apply(&mut apply_ctx);
        // Consume child to release borrow on sandbox
        let child_changes = child.into_changes();

        let (result, should_commit) = match handler_result {
            Ok(result) => {
                // tes: merge child mutations into parent
                sandbox.merge_child_changes(child_changes);
                (result, true)
            }
            Err(result) if result.is_claimed() => {
                // tec: discard child mutations, keep fee deduction
                (result, true)
            }
            Err(result) => {
                // tem/tef/ter: discard everything
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

        // 9. Commit to ledger
        if should_commit {
            // Build metadata before consuming changes
            let meta = changes.build_metadata(0, result.code());

            changes.apply_to_ledger(ledger)?;

            // Record transaction + metadata in tx_map
            let tx_hash = rxrpl_protocol::tx::compute_tx_hash(tx)
                .map_err(|e| TxEngineError::Codec(e.to_string()))?;
            let tx_record = serde_json::json!({
                "tx_json": tx,
                "result": result.as_str(),
                "meta": {
                    "TransactionIndex": meta.tx_index,
                    "TransactionResult": result.as_str(),
                    "AffectedNodes": meta.affected_nodes.len(),
                },
            });
            let tx_data = serde_json::to_vec(&tx_record)
                .map_err(|e| TxEngineError::Codec(e.to_string()))?;
            ledger.add_transaction(tx_hash, tx_data)?;
        }

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

    /// A test transactor that fails with tec (claimed cost).
    struct TecTransactor;

    impl Transactor for TecTransactor {
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
            Err(TransactionResult::TecClaimCost)
        }
    }

    fn test_engine_with(tx_type: TransactionType, transactor: impl Transactor + 'static) -> TxEngine {
        let mut registry = TransactorRegistry::new();
        registry.register(tx_type, transactor);
        TxEngine::new_without_sig_check(registry)
    }

    fn make_tx(tx_type: &str) -> Value {
        serde_json::json!({
            "TransactionType": tx_type,
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10"
        })
    }

    fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let data = serde_json::to_vec(&account).unwrap();
        ledger.put_state(key, data).unwrap();
        ledger
    }

    #[test]
    fn engine_noop_success() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
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

    #[test]
    fn fee_deducted_from_sender() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify fee was deducted
        let account_id = decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
        let key = keylet::account(&account_id);
        let data = ledger.get_state(&key).unwrap();
        let account: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(account["Balance"].as_str().unwrap(), "999990");
    }

    #[test]
    fn tec_keeps_fee_discards_mutations() {
        let engine = test_engine_with(TransactionType::AccountSet, TecTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TecClaimCost);

        // Fee should still be deducted
        let account_id = decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh").unwrap();
        let key = keylet::account(&account_id);
        let data = ledger.get_state(&key).unwrap();
        let account: Value = serde_json::from_slice(data).unwrap();
        assert_eq!(account["Balance"].as_str().unwrap(), "999990");
    }

    #[test]
    fn insufficient_fee_balance() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 5);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TerInsufFee);
    }

    #[test]
    fn tx_recorded_in_tx_map() {
        let engine = test_engine_with(TransactionType::AccountSet, NoopTransactor);
        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let tx = make_tx("AccountSet");

        engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();

        // Verify tx was recorded
        let tx_hash = rxrpl_protocol::tx::compute_tx_hash(&tx).unwrap();
        assert!(ledger.tx_map.has(&tx_hash));
    }

    #[test]
    fn signature_verification_rejects_invalid() {
        let mut registry = TransactorRegistry::new();
        registry.register(TransactionType::AccountSet, NoopTransactor);
        // Engine WITH signature verification enabled
        let engine = TxEngine::new(registry);

        let mut ledger = setup_ledger_with_account("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        // Unsigned tx -- should fail sig check
        let tx = make_tx("AccountSet");

        let result = engine.apply(&tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TefBadSignature);
    }

    #[test]
    fn signature_verification_accepts_valid() {
        use rxrpl_codec::address::classic::encode_classic_address_from_pubkey;
        use rxrpl_crypto::{KeyPair, KeyType, Seed};

        let seed = Seed::from_passphrase("test_engine_sig");
        let kp = KeyPair::from_seed(&seed, KeyType::Ed25519);
        let sender = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

        let mut registry = TransactorRegistry::new();
        registry.register(TransactionType::AccountSet, NoopTransactor);
        let engine = TxEngine::new(registry);

        let mut ledger = setup_ledger_with_account(&sender, 1_000_000);
        let rules = Rules::new();
        let fees = FeeSettings::default();

        let tx = serde_json::json!({
            "TransactionType": "AccountSet",
            "Account": sender,
            "Fee": "10",
            "Sequence": 1,
        });
        let signed_tx = rxrpl_protocol::tx::sign(&tx, &kp).unwrap();

        let result = engine.apply(&signed_tx, &mut ledger, &rules, &fees).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);
    }
}
