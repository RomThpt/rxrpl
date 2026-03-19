use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: MPTokenIssuance and MPToken lifecycle.
///
/// - MPTokenIssuance can only be created by MPTokenIssuanceCreate.
/// - MPToken can only be created by MPTokenAuthorize.
/// - MPTokenIssuance can only be deleted by MPTokenIssuanceDestroy.
/// - MPToken can only be deleted by MPTokenAuthorize (or Clawback).
pub struct ValidMptIssuance;

impl InvariantCheck for ValidMptIssuance {
    fn name(&self) -> &str {
        "ValidMptIssuance"
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

        // Check insertions
        for (key, data) in &changes.inserts {
            if let Ok(obj) = serde_json::from_slice::<Value>(data) {
                let entry_type = obj.get("LedgerEntryType").and_then(|v| v.as_str());

                if entry_type == Some("MPTokenIssuance") && tx_type != "MPTokenIssuanceCreate" {
                    return Err(format!(
                        "MPTokenIssuance created at {key} by {tx_type} (expected MPTokenIssuanceCreate)"
                    ));
                }

                if entry_type == Some("MPToken") && tx_type != "MPTokenAuthorize" {
                    return Err(format!(
                        "MPToken created at {key} by {tx_type} (expected MPTokenAuthorize)"
                    ));
                }
            }
        }

        // Check deletions
        for (key, data) in &changes.deletes {
            if let Ok(obj) = serde_json::from_slice::<Value>(data) {
                let entry_type = obj.get("LedgerEntryType").and_then(|v| v.as_str());

                if entry_type == Some("MPTokenIssuance") && tx_type != "MPTokenIssuanceDestroy" {
                    return Err(format!(
                        "MPTokenIssuance deleted at {key} by {tx_type} (expected MPTokenIssuanceDestroy)"
                    ));
                }

                if entry_type == Some("MPToken")
                    && tx_type != "MPTokenAuthorize"
                    && tx_type != "Clawback"
                {
                    return Err(format!(
                        "MPToken deleted at {key} by {tx_type} (expected MPTokenAuthorize or Clawback)"
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

    fn mpt_issuance() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "MPTokenIssuance",
        }))
        .unwrap()
    }

    fn mptoken() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "MPToken",
        }))
        .unwrap()
    }

    #[test]
    fn mpt_issuance_create_passes() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), mpt_issuance());
        let tx = json!({ "TransactionType": "MPTokenIssuanceCreate" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn mpt_issuance_wrong_tx_fails() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), mpt_issuance());
        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn mptoken_authorize_create_passes() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes.inserts.insert(Hash256::new([0x01; 32]), mptoken());
        let tx = json!({ "TransactionType": "MPTokenAuthorize" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn mptoken_wrong_create_tx_fails() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes.inserts.insert(Hash256::new([0x01; 32]), mptoken());
        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn mpt_issuance_destroy_passes() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes
            .deletes
            .insert(Hash256::new([0x01; 32]), mpt_issuance());
        let tx = json!({ "TransactionType": "MPTokenIssuanceDestroy" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn mpt_issuance_wrong_delete_fails() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes
            .deletes
            .insert(Hash256::new([0x01; 32]), mpt_issuance());
        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn mptoken_clawback_delete_passes() {
        let check = ValidMptIssuance;
        let mut changes = empty_changes();
        changes.deletes.insert(Hash256::new([0x01; 32]), mptoken());
        let tx = json!({ "TransactionType": "Clawback" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }
}
