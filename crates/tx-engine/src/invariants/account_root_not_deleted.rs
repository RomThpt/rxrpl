use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: AccountRoot entries may only be deleted if balance == "0" and OwnerCount == 0.
///
/// This permits AccountDelete while catching accidental deletions with
/// non-zero balances or outstanding owned objects.
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
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in &changes.deletes {
            if let Ok(obj) = serde_json::from_slice::<serde_json::Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) == Some("AccountRoot") {
                    let balance = obj.get("Balance").and_then(|v| v.as_str()).unwrap_or("0");
                    let owner_count = obj.get("OwnerCount").and_then(|v| v.as_u64()).unwrap_or(0);
                    if balance != "0" || owner_count != 0 {
                        return Err(format!(
                            "AccountRoot at {key} deleted with non-zero balance ({balance}) or owners ({owner_count})"
                        ));
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
        assert!(check.check(&empty_changes(), 100, 100, None).is_ok());
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
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn delete_account_root_zero_balance_passes() {
        let check = AccountRootNotDeleted;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "0",
            "OwnerCount": 0,
        }))
        .unwrap();
        changes.deletes.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn delete_account_root_nonzero_balance_fails() {
        let check = AccountRootNotDeleted;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "1000",
            "OwnerCount": 0,
        }))
        .unwrap();
        changes.deletes.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn delete_account_root_nonzero_owners_fails() {
        let check = AccountRootNotDeleted;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "0",
            "OwnerCount": 1,
        }))
        .unwrap();
        changes.deletes.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }
}
