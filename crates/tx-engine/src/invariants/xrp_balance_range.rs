use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Maximum XRP supply in drops: 100 billion XRP = 100_000_000_000 * 1_000_000 drops.
const MAX_XRP_DROPS: u64 = 100_000_000_000_000_000;

/// Invariant: every AccountRoot balance must be in [0, MAX_XRP_DROPS].
///
/// Complements `NoNegativeBalance` by also enforcing the upper bound.
pub struct XrpBalanceRange;

impl InvariantCheck for XrpBalanceRange {
    fn name(&self) -> &str {
        "XrpBalanceRange"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.updates.iter().chain(changes.inserts.iter()) {
            if let Ok(obj) = serde_json::from_slice::<Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("AccountRoot") {
                    continue;
                }

                if let Some(balance_str) = obj.get("Balance").and_then(|v| v.as_str()) {
                    if balance_str.starts_with('-') {
                        return Err(format!(
                            "account at {key} has negative balance: {balance_str}"
                        ));
                    }
                    let drops: u64 = balance_str.parse().map_err(|_| {
                        format!("account at {key} has unparseable balance: {balance_str}")
                    })?;
                    if drops > MAX_XRP_DROPS {
                        return Err(format!(
                            "account at {key} has balance {drops} exceeding max {MAX_XRP_DROPS}"
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
    fn valid_balance_passes() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("1000000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn zero_balance_passes() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("0"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn max_balance_passes() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        changes.updates.insert(
            Hash256::new([0x01; 32]),
            account_bytes(&MAX_XRP_DROPS.to_string()),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn exceeds_max_balance_fails() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        changes.updates.insert(
            Hash256::new([0x01; 32]),
            account_bytes(&(MAX_XRP_DROPS + 1).to_string()),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn negative_balance_fails() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), account_bytes("-100"));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_account_root_ignored() {
        let check = XrpBalanceRange;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "Balance": "999999999999999999",
        }))
        .unwrap();
        changes.updates.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
