use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: the XRP destroyed must equal the transaction Fee.
///
/// For non-pseudo transactions, the drops destroyed during processing
/// must exactly match the Fee field in the transaction JSON.
/// Pseudo-transactions (tx=None) are skipped.
pub struct TransactionFeeCheck;

impl InvariantCheck for TransactionFeeCheck {
    fn name(&self) -> &str {
        "TransactionFeeCheck"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        tx: Option<&Value>,
    ) -> Result<(), String> {
        let tx = match tx {
            Some(t) => t,
            None => return Ok(()), // pseudo-transaction, skip
        };

        let fee_str = tx.get("Fee").and_then(|v| v.as_str()).unwrap_or("0");

        let total_fee: u64 = fee_str
            .parse()
            .map_err(|_| format!("transaction has invalid Fee: {fee_str}"))?;

        // For BatchSubmit, inner fees are destroyed on success but not on failure.
        // Accept either outer_fee alone (batch failed) or outer_fee + inner_fees (batch succeeded).
        if tx.get("TransactionType").and_then(|v| v.as_str()) == Some("BatchSubmit") {
            let mut total_with_inner = total_fee;
            if let Some(raw_txs) = tx.get("RawTransactions").and_then(|v| v.as_array()) {
                for raw in raw_txs {
                    if let Some(inner_fee_str) = raw
                        .pointer("/RawTransaction/InnerTx/Fee")
                        .and_then(|v| v.as_str())
                    {
                        let inner_fee: u64 = inner_fee_str.parse().unwrap_or(0);
                        total_with_inner += inner_fee;
                    }
                }
            }
            // Valid: outer_fee only (batch failed) or outer + inner fees (batch succeeded)
            if changes.destroyed_drops != total_fee && changes.destroyed_drops != total_with_inner {
                return Err(format!(
                    "destroyed_drops ({}) != outer Fee ({total_fee}) or total ({total_with_inner})",
                    changes.destroyed_drops
                ));
            }
            return Ok(());
        }

        if changes.destroyed_drops != total_fee {
            return Err(format!(
                "destroyed_drops ({}) != Fee ({total_fee})",
                changes.destroyed_drops
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_changes(destroyed: u64) -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: destroyed,
        }
    }

    #[test]
    fn fee_matches_destroyed_passes() {
        let check = TransactionFeeCheck;
        let tx = serde_json::json!({ "Fee": "12" });
        let changes = empty_changes(12);
        assert!(check.check(&changes, 100, 88, Some(&tx)).is_ok());
    }

    #[test]
    fn fee_mismatch_fails() {
        let check = TransactionFeeCheck;
        let tx = serde_json::json!({ "Fee": "12" });
        let changes = empty_changes(15);
        assert!(check.check(&changes, 100, 85, Some(&tx)).is_err());
    }

    #[test]
    fn pseudo_tx_none_passes() {
        let check = TransactionFeeCheck;
        let changes = empty_changes(0);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn missing_fee_treated_as_zero() {
        let check = TransactionFeeCheck;
        let tx = serde_json::json!({ "TransactionType": "SetFee" });
        let changes = empty_changes(0);
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }
}
