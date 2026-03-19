use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Known ledger entry types in the XRPL protocol.
const KNOWN_ENTRY_TYPES: &[&str] = &[
    "AccountRoot",
    "Amendments",
    "AMM",
    "Bridge",
    "Check",
    "Credential",
    "DID",
    "DepositPreauth",
    "DirectoryNode",
    "Escrow",
    "FeeSettings",
    "LedgerHashes",
    "MPToken",
    "MPTokenIssuance",
    "NFTokenOffer",
    "NFTokenPage",
    "NegativeUNL",
    "Offer",
    "Oracle",
    "PayChannel",
    "PermissionedDomain",
    "RippleState",
    "SignerList",
    "Ticket",
    "Vault",
    "XChainOwnedClaimID",
    "XChainOwnedCreateAccountClaimID",
];

/// Invariant: all inserted entries must have a valid LedgerEntryType.
///
/// Catches bugs where a transactor creates a ledger entry with an
/// invalid or missing type field.
pub struct ValidLedgerEntryType;

impl InvariantCheck for ValidLedgerEntryType {
    fn name(&self) -> &str {
        "ValidLedgerEntryType"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in &changes.inserts {
            let obj: serde_json::Value = serde_json::from_slice(data)
                .map_err(|e| format!("insert at {key} is not valid JSON: {e}"))?;

            let entry_type = obj
                .get("LedgerEntryType")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("insert at {key} missing LedgerEntryType field"))?;

            if !KNOWN_ENTRY_TYPES.contains(&entry_type) {
                return Err(format!(
                    "insert at {key} has unknown LedgerEntryType: {entry_type}"
                ));
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

    #[test]
    fn valid_entry_type_passes() {
        let check = ValidLedgerEntryType;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn unknown_entry_type_fails() {
        let check = ValidLedgerEntryType;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "BogusType",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn missing_entry_type_fails() {
        let check = ValidLedgerEntryType;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "Balance": "1000",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn no_inserts_passes() {
        let check = ValidLedgerEntryType;
        assert!(check.check(&empty_changes(), 100, 100, None).is_ok());
    }

    #[test]
    fn updates_not_checked() {
        let check = ValidLedgerEntryType;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "BogusType",
        }))
        .unwrap();
        // Only inserts are checked, updates are not
        changes.updates.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
