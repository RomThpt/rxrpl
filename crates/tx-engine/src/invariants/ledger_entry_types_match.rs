use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: an update must never change the LedgerEntryType of an entry.
///
/// If an entry's type mutates across an update, a transactor has a bug
/// (e.g. writing the wrong object back under an existing key).
pub struct LedgerEntryTypesMatch;

impl InvariantCheck for LedgerEntryTypesMatch {
    fn name(&self) -> &str {
        "LedgerEntryTypesMatch"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, new_data) in &changes.updates {
            let original_data = match changes.originals.get(key) {
                Some(d) => d,
                None => continue,
            };

            let new_obj: serde_json::Value = serde_json::from_slice(new_data)
                .map_err(|e| format!("update at {key} is not valid JSON: {e}"))?;
            let old_obj: serde_json::Value = serde_json::from_slice(original_data)
                .map_err(|e| format!("original at {key} is not valid JSON: {e}"))?;

            let new_type = new_obj.get("LedgerEntryType").and_then(|v| v.as_str());
            let old_type = old_obj.get("LedgerEntryType").and_then(|v| v.as_str());

            if new_type != old_type {
                return Err(format!(
                    "entry at {key} changed LedgerEntryType from {:?} to {:?}",
                    old_type, new_type
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
    fn same_type_passes() {
        let check = LedgerEntryTypesMatch;
        let mut changes = empty_changes();
        let key = Hash256::new([0x01; 32]);
        let old = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "1000",
        }))
        .unwrap();
        let new = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "900",
        }))
        .unwrap();
        changes.originals.insert(key, old);
        changes.updates.insert(key, new);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn type_changed_fails() {
        let check = LedgerEntryTypesMatch;
        let mut changes = empty_changes();
        let key = Hash256::new([0x01; 32]);
        let old = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
        }))
        .unwrap();
        let new = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.originals.insert(key, old);
        changes.updates.insert(key, new);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn no_updates_passes() {
        let check = LedgerEntryTypesMatch;
        assert!(check.check(&empty_changes(), 100, 100, None).is_ok());
    }
}
