use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: newly inserted AccountRoot entries must have valid initial state.
///
/// A new account must have: Account present, Balance parseable, and Sequence > 0.
pub struct ValidNewAccountRoot;

impl InvariantCheck for ValidNewAccountRoot {
    fn name(&self) -> &str {
        "ValidNewAccountRoot"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in &changes.inserts {
            let obj: serde_json::Value = match serde_json::from_slice(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("AccountRoot") {
                continue;
            }

            // Account must be present and non-empty
            let account = obj
                .get("Account")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("new AccountRoot at {key} missing Account field"))?;
            if account.is_empty() {
                return Err(format!("new AccountRoot at {key} has empty Account"));
            }

            // Balance must be parseable as u64
            let balance_str = obj
                .get("Balance")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("new AccountRoot at {key} missing Balance field"))?;
            let _balance: u64 = balance_str.parse().map_err(|_| {
                format!("new AccountRoot at {key} has invalid Balance: {balance_str}")
            })?;

            // Pseudo-accounts (AMM / Vault) carry a designator field and, under
            // featureSingleAssetVault, a Sequence of 0 (serialized as absent).
            let is_pseudo = obj.get("AMMID").is_some() || obj.get("VaultID").is_some();

            // A regular new account must have Sequence > 0; a pseudo-account may
            // omit it (Sequence 0).
            let sequence = obj.get("Sequence").and_then(|v| v.as_u64());
            if !is_pseudo {
                match sequence {
                    Some(s) if s > 0 => {}
                    Some(_) => return Err(format!("new AccountRoot at {key} has Sequence=0")),
                    None => return Err(format!("new AccountRoot at {key} missing Sequence field")),
                }
            }

            // OwnerCount is not constrained at creation: rippled's
            // ValidNewAccountRoot allows a freshly created (pseudo-)account to
            // own objects (e.g. an AMM account that holds its pool trust lines).
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

    fn account_root(account: &str, balance: &str, sequence: u64, owner_count: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": account,
            "Balance": balance,
            "Sequence": sequence,
            "OwnerCount": owner_count,
        }))
        .unwrap()
    }

    #[test]
    fn valid_new_account_passes() {
        let check = ValidNewAccountRoot;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            account_root("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", "1000000", 1, 0),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn nonzero_owner_count_allowed() {
        // A pseudo-account (e.g. an AMM) may own objects at creation.
        let check = ValidNewAccountRoot;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            account_root("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", "1000000", 1, 3),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn missing_account_fails() {
        let check = ValidNewAccountRoot;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "1000000",
            "Sequence": 1,
            "OwnerCount": 0,
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn sequence_zero_fails() {
        let check = ValidNewAccountRoot;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            account_root("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", "1000000", 0, 0),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn updates_not_checked() {
        let check = ValidNewAccountRoot;
        let mut changes = empty_changes();
        // OwnerCount > 0 in an update is fine (not a new account)
        changes.updates.insert(
            Hash256::new([0x01; 32]),
            account_root("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh", "1000000", 1, 5),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
