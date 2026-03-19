use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: zombie trust lines (RippleState with all-zero values) must be deleted.
///
/// A RippleState with Balance=0, LowLimit=0, and HighLimit=0 serves no
/// purpose and should have been removed by the transactor. Leaving them
/// bloats the ledger.
pub struct NoZeroBalanceEntries;

impl NoZeroBalanceEntries {
    fn is_zero_value(amount: &serde_json::Value) -> bool {
        if let Some(obj) = amount.as_object() {
            obj.get("value")
                .and_then(|v| v.as_str())
                .map(|s| s == "0" || s == "0.0" || s == "-0")
                .unwrap_or(false)
        } else {
            false
        }
    }
}

impl InvariantCheck for NoZeroBalanceEntries {
    fn name(&self) -> &str {
        "NoZeroBalanceEntries"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.inserts.iter().chain(changes.updates.iter()) {
            if let Ok(obj) = serde_json::from_slice::<serde_json::Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("RippleState") {
                    continue;
                }

                let balance_zero = obj
                    .get("Balance")
                    .map(|b| Self::is_zero_value(b))
                    .unwrap_or(false);
                let low_zero = obj
                    .get("LowLimit")
                    .map(|l| Self::is_zero_value(l))
                    .unwrap_or(false);
                let high_zero = obj
                    .get("HighLimit")
                    .map(|h| Self::is_zero_value(h))
                    .unwrap_or(false);

                if balance_zero && low_zero && high_zero {
                    return Err(format!(
                        "RippleState at {key} has zero balance and zero limits -- should be deleted"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;
    use std::collections::HashMap;

    fn empty_changes() -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        }
    }

    fn ripple_state(balance: &str, low: &str, high: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": { "currency": "USD", "issuer": "rIssuer", "value": balance },
            "LowLimit": { "currency": "USD", "issuer": "rA", "value": low },
            "HighLimit": { "currency": "USD", "issuer": "rB", "value": high },
        }))
        .unwrap()
    }

    #[test]
    fn nonzero_balance_passes() {
        let check = NoZeroBalanceEntries;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), ripple_state("100", "0", "1000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn zero_balance_but_limit_nonzero_passes() {
        let check = NoZeroBalanceEntries;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), ripple_state("0", "0", "1000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn all_zeros_fails() {
        let check = NoZeroBalanceEntries;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), ripple_state("0", "0", "0"));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }
}
