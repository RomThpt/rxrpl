use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: NFTokenPage entries must not be empty after a transaction.
///
/// An NFTokenPage with an empty or missing NFTokens array indicates a bug
/// in the mint/burn handlers. Empty pages should be deleted, not left behind.
pub struct NFTokenPageBounds;

impl InvariantCheck for NFTokenPageBounds {
    fn name(&self) -> &str {
        "NFTokenPageBounds"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.inserts.iter().chain(changes.updates.iter()) {
            let obj = match serde_json::from_slice::<Value>(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("NFTokenPage") {
                continue;
            }

            let tokens = obj
                .get("NFTokens")
                .and_then(|v| v.as_array())
                .ok_or_else(|| format!("NFTokenPage at {key} missing NFTokens array"))?;

            if tokens.is_empty() {
                return Err(format!(
                    "NFTokenPage at {key} has empty NFTokens array (should be deleted)"
                ));
            }

            // Each token entry must have an NFTokenID
            for (i, entry) in tokens.iter().enumerate() {
                if entry.get("NFTokenID").and_then(|v| v.as_str()).is_none() {
                    return Err(format!("NFTokenPage at {key} entry {i} missing NFTokenID"));
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

    #[test]
    fn valid_nftoken_page_passes() {
        let check = NFTokenPageBounds;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "NFTokenPage",
            "NFTokens": [
                { "NFTokenID": "00080000A0C8B8C5D2F8A1B3E4D5F6A7B8C9D0E1F2A3B4C5D6E7F8091A2B3C" }
            ],
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn empty_nftoken_page_fails() {
        let check = NFTokenPageBounds;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "NFTokenPage",
            "NFTokens": [],
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn missing_nftokens_array_fails() {
        let check = NFTokenPageBounds;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "NFTokenPage",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn missing_nftoken_id_fails() {
        let check = NFTokenPageBounds;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "NFTokenPage",
            "NFTokens": [
                { "Flags": 0 }
            ],
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_nftoken_page_ignored() {
        let check = NFTokenPageBounds;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
