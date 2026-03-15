use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;

/// Invariant: AccountRoot entries must not be deleted.
///
/// AccountDelete is not yet implemented, so any AccountRoot in the
/// deletes set indicates a bug in transactor logic.
pub struct AccountRootNotDeleted;

impl InvariantCheck for AccountRootNotDeleted {
    fn name(&self) -> &str {
        "AccountRootNotDeleted"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
    ) -> Result<(), String> {
        for (key, data) in &changes.deletes {
            if let Ok(obj) = serde_json::from_slice::<serde_json::Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("AccountRoot") {
                    return Err(format!("AccountRoot at {key} was deleted"));
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

    #[test]
    fn no_deletes_passes() {
        let check = AccountRootNotDeleted;
        assert!(check.check(&empty_changes(), 100, 100).is_ok());
    }

    #[test]
    fn delete_non_account_passes() {
        let check = AccountRootNotDeleted;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
        }))
        .unwrap();
        changes.deletes.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100).is_ok());
    }

    #[test]
    fn delete_account_root_fails() {
        let check = AccountRootNotDeleted;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.deletes.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100).is_err());
    }
}
