use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: no account may have a negative XRP balance.
///
/// Scans all modified and inserted AccountRoots to verify that the
/// Balance field is non-negative.
pub struct NoNegativeBalance;

impl InvariantCheck for NoNegativeBalance {
    fn name(&self) -> &str {
        "NoNegativeBalance"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.updates.iter().chain(changes.inserts.iter()) {
            if let Ok(obj) = serde_json::from_slice::<serde_json::Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("AccountRoot") {
                    if let Some(balance_str) = obj.get("Balance").and_then(|v| v.as_str()) {
                        // Balance is stored as a string of drops
                        if balance_str.starts_with('-') {
                            return Err(format!(
                                "account at {key} has negative balance: {balance_str}"
                            ));
                        }
                    }
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

    fn account_bytes(balance: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": balance,
        }))
        .unwrap()
    }

    fn empty_changes() -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        }
    }

    #[test]
    fn positive_balance_passes() {
        let check = NoNegativeBalance;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("1000000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn zero_balance_passes() {
        let check = NoNegativeBalance;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("0"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn negative_balance_fails() {
        let check = NoNegativeBalance;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("-100"));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_account_root_ignored() {
        let check = NoNegativeBalance;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "Balance": "-100",
        }))
        .unwrap();
        changes.updates.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
