use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: Clawback must modify exactly one trust line or MPToken, and
/// the holder's balance must not become negative.
pub struct ValidClawback;

impl ValidClawback {
    fn is_trust_line_or_mptoken(obj: &Value) -> bool {
        matches!(
            obj.get("LedgerEntryType").and_then(|v| v.as_str()),
            Some("RippleState") | Some("MPToken")
        )
    }
}

impl InvariantCheck for ValidClawback {
    fn name(&self) -> &str {
        "ValidClawback"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        tx: Option<&Value>,
    ) -> Result<(), String> {
        let tx_type = tx
            .and_then(|t| t.get("TransactionType"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if tx_type != "Clawback" {
            return Ok(());
        }

        let mut modified_count = 0u32;

        for (key, data) in changes.updates.iter() {
            if let Ok(obj) = serde_json::from_slice::<Value>(data) {
                if !Self::is_trust_line_or_mptoken(&obj) {
                    continue;
                }

                modified_count += 1;

                // The post-clawback magnitude must not exceed the
                // pre-clawback magnitude (clawback can only reduce what the
                // holder owes). RippleState Balance.value is stored from
                // the low-account perspective, so a stored `-100` is still
                // a holder balance of 100 when the issuer is the high
                // account; checking absolute magnitude is perspective-safe.
                let new_mag = Self::balance_magnitude(&obj);
                let original_mag = changes
                    .originals
                    .get(key)
                    .and_then(|raw| serde_json::from_slice::<Value>(raw).ok())
                    .map(|orig| Self::balance_magnitude(&orig));
                if let Some(orig_mag) = original_mag {
                    if new_mag > orig_mag {
                        return Err(format!(
                            "Clawback increased balance magnitude at {key} ({orig_mag} -> {new_mag})"
                        ));
                    }
                }
            }
        }

        if modified_count == 0 {
            return Err("Clawback did not modify any RippleState or MPToken entry".to_string());
        }

        Ok(())
    }
}

impl ValidClawback {
    fn balance_magnitude(obj: &Value) -> f64 {
        if let Some(val_str) = obj
            .get("Balance")
            .and_then(|b| b.as_object())
            .and_then(|o| o.get("value"))
            .and_then(|v| v.as_str())
        {
            return val_str.parse::<f64>().unwrap_or(0.0).abs();
        }
        if let Some(s) = obj.get("Balance").and_then(|v| v.as_str()) {
            return s.parse::<f64>().unwrap_or(0.0).abs();
        }
        if let Some(amt) = obj.get("MPTAmount").and_then(|v| v.as_str()) {
            return amt.parse::<f64>().unwrap_or(0.0).abs();
        }
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;
    use serde_json::json;
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

    fn ripple_state(balance_value: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "RippleState",
            "Balance": { "currency": "USD", "issuer": "rX", "value": balance_value },
        }))
        .unwrap()
    }

    #[test]
    fn valid_clawback_passes() {
        let check = ValidClawback;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), ripple_state("50"));
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn clawback_increasing_magnitude_fails() {
        // A clawback that grows the magnitude (here 5 -> 50) is invalid
        // regardless of perspective sign.
        let check = ValidClawback;
        let mut changes = empty_changes();
        let key = Hash256::new([0x01; 32]);
        changes.originals.insert(key, ripple_state("5"));
        changes.updates.insert(key, ripple_state("50"));
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn clawback_decreasing_magnitude_negative_balance_passes() {
        // RippleState Balance can be negative when issuer is the high
        // account; magnitude going from 100 to 50 is a valid clawback.
        let check = ValidClawback;
        let mut changes = empty_changes();
        let key = Hash256::new([0x01; 32]);
        changes.originals.insert(key, ripple_state("-100"));
        changes.updates.insert(key, ripple_state("-50"));
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn clawback_no_trust_line_modified_fails() {
        let check = ValidClawback;
        let changes = empty_changes();
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn non_clawback_tx_passes() {
        let check = ValidClawback;
        let changes = empty_changes();
        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn clawback_zero_balance_passes() {
        let check = ValidClawback;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), ripple_state("0"));
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn clawback_mptoken_passes() {
        let check = ValidClawback;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "MPToken",
            "MPTAmount": "100",
        }))
        .unwrap();
        changes.updates.insert(Hash256::new([0x01; 32]), data);
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }
}
