use rxrpl_protocol::{TransactionResult, TransactionType};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// Maximum number of inner transactions in a batch.
const MAX_BATCH_SIZE: usize = 8;

/// BatchSubmit transaction handler.
///
/// Validates the outer batch structure. Actual inner-transaction execution
/// is handled by `TxEngine::apply_batch` which has access to the registry.
pub struct BatchSubmitTransactor;

/// Extract the inner transaction list from a BatchSubmit tx.
///
/// Each entry is expected to be `{ "RawTransaction": { "InnerTx": { ... } } }`.
pub fn extract_inner_txs(tx: &Value) -> Result<Vec<&Value>, TransactionResult> {
    let raw_txs = tx
        .get("RawTransactions")
        .and_then(|v| v.as_array())
        .ok_or(TransactionResult::TemMalformed)?;

    if raw_txs.is_empty() || raw_txs.len() > MAX_BATCH_SIZE {
        return Err(TransactionResult::TemMalformed);
    }

    let mut inner_txs = Vec::with_capacity(raw_txs.len());
    for raw in raw_txs {
        let inner = raw
            .get("RawTransaction")
            .and_then(|v| v.get("InnerTx"))
            .ok_or(TransactionResult::TemMalformed)?;

        // Inner tx must have TransactionType and Account
        let inner_type = inner
            .get("TransactionType")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemMalformed)?;

        // No nested batches
        if inner_type == "BatchSubmit" {
            return Err(TransactionResult::TemMalformed);
        }

        // Must be a known transaction type
        TransactionType::from_name(inner_type).map_err(|_| TransactionResult::TemMalformed)?;

        inner
            .get("Account")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemMalformed)?;

        inner_txs.push(inner);
    }

    Ok(inner_txs)
}

impl Transactor for BatchSubmitTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Validate outer Account
        helpers::get_account(ctx.tx)?;

        // Validate inner transactions structure
        extract_inner_txs(ctx.tx)?;

        Ok(())
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // Inner tx preclaim is handled by the engine during recursive execution
        Ok(())
    }

    fn calculate_base_fee(&self, ctx: &PreflightContext<'_>) -> u64 {
        // Base fee * number of inner transactions
        let count = ctx
            .tx
            .get("RawTransactions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(1) as u64;
        ctx.default_base_fee() * count
    }

    fn apply(&self, _ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        // Inner tx execution is handled by TxEngine::apply_batch.
        // This method is not called for BatchSubmit.
        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use rxrpl_amendment::Rules;
    use serde_json::json;

    fn make_batch(inner_txs: Vec<Value>) -> Value {
        let raw: Vec<Value> = inner_txs
            .into_iter()
            .map(|tx| json!({ "RawTransaction": { "InnerTx": tx } }))
            .collect();
        json!({
            "TransactionType": "BatchSubmit",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "20",
            "RawTransactions": raw,
        })
    }

    fn inner_payment() -> Value {
        json!({
            "TransactionType": "Payment",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1,
        })
    }

    #[test]
    fn preflight_valid_batch() {
        let tx = make_batch(vec![inner_payment()]);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(BatchSubmitTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn preflight_missing_raw_transactions() {
        let tx = json!({
            "TransactionType": "BatchSubmit",
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
            BatchSubmitTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_empty_raw_transactions() {
        let tx = json!({
            "TransactionType": "BatchSubmit",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
            "RawTransactions": [],
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            BatchSubmitTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_nested_batch_rejected() {
        let nested = json!({
            "TransactionType": "BatchSubmit",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Fee": "10",
            "RawTransactions": [],
        });
        let tx = make_batch(vec![nested]);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            BatchSubmitTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_too_many_inner_txs() {
        let txs: Vec<Value> = (0..9).map(|_| inner_payment()).collect();
        let tx = make_batch(txs);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            BatchSubmitTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_inner_missing_account() {
        let bad = json!({
            "TransactionType": "Payment",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
        });
        let tx = make_batch(vec![bad]);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            BatchSubmitTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn calculate_base_fee_scales_with_count() {
        let tx = make_batch(vec![inner_payment(), inner_payment(), inner_payment()]);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        // 3 inner txs * base_fee(10)
        assert_eq!(BatchSubmitTransactor.calculate_base_fee(&ctx), 30);
    }
}
