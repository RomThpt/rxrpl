use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: XRP cannot appear in trust lines (RippleState entries).
///
/// XRP is the native asset and is tracked via AccountRoot balances,
/// never via RippleState entries. A RippleState with currency "XRP"
/// or a raw string amount (drops format) indicates a transactor bug.
pub struct NoXrpTrustLines;

impl NoXrpTrustLines {
    fn check_amount_not_xrp(
        amount: &serde_json::Value,
        field: &str,
        key: &impl std::fmt::Display,
    ) -> Result<(), String> {
        // If amount is a string, it's XRP drops format -- invalid for trust lines
        if amount.is_string() {
            return Err(format!(
                "RippleState at {key} has {field} as string (XRP drops format)"
            ));
        }
        // If amount is an object, check currency field
        if let Some(currency) = amount.get("currency").and_then(|v| v.as_str()) {
            if currency == "XRP" {
                return Err(format!(
                    "RippleState at {key} has {field}.currency == \"XRP\""
                ));
            }
        }
        Ok(())
    }
}

impl InvariantCheck for NoXrpTrustLines {
    fn name(&self) -> &str {
        "NoXrpTrustLines"
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
                if let Some(low) = obj.get("LowLimit") {
                    Self::check_amount_not_xrp(low, "LowLimit", key)?;
                }
                if let Some(high) = obj.get("HighLimit") {
                    Self::check_amount_not_xrp(high, "HighLimit", key)?;
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

    fn ripple_state_bytes(low_limit: serde_json::Value, high_limit: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": { "currency": "USD", "issuer": "rIssuer", "value": "100" },
            "LowLimit": low_limit,
            "HighLimit": high_limit,
        }))
        .unwrap()
    }

    #[test]
    fn valid_iou_trust_line_passes() {
        let check = NoXrpTrustLines;
        let mut changes = empty_changes();
        let data = ripple_state_bytes(
            serde_json::json!({ "currency": "USD", "issuer": "rA", "value": "0" }),
            serde_json::json!({ "currency": "USD", "issuer": "rB", "value": "1000" }),
        );
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn low_limit_xrp_currency_fails() {
        let check = NoXrpTrustLines;
        let mut changes = empty_changes();
        let data = ripple_state_bytes(
            serde_json::json!({ "currency": "XRP", "issuer": "rA", "value": "0" }),
            serde_json::json!({ "currency": "USD", "issuer": "rB", "value": "1000" }),
        );
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn high_limit_string_format_fails() {
        let check = NoXrpTrustLines;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Balance": { "currency": "USD", "issuer": "rIssuer", "value": "100" },
            "LowLimit": { "currency": "USD", "issuer": "rA", "value": "0" },
            "HighLimit": "1000000",
        }))
        .unwrap();
        changes.updates.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_ripple_state_ignored() {
        let check = NoXrpTrustLines;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "LowLimit": "1000000",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
