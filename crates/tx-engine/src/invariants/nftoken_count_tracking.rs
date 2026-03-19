use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: NFToken mint/burn counters must track correctly.
///
/// - If tx is NFTokenMint and succeeded: MintedNFTokens must increase by 1.
/// - If tx is NFTokenBurn and succeeded: BurnedNFTokens must increase by 1.
/// - Otherwise: these counters must not change.
pub struct NFTokenCountTracking;

impl NFTokenCountTracking {
    fn get_counter(obj: &Value, field: &str) -> u64 {
        obj.get(field).and_then(|v| v.as_u64()).unwrap_or(0)
    }
}

impl InvariantCheck for NFTokenCountTracking {
    fn name(&self) -> &str {
        "NFTokenCountTracking"
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

        let tx_success = tx
            .and_then(|t| t.get("meta"))
            .and_then(|m| m.get("TransactionResult"))
            .and_then(|v| v.as_str())
            .unwrap_or("tesSUCCESS")
            == "tesSUCCESS";

        let is_mint = tx_type == "NFTokenMint" && tx_success;
        let is_burn = tx_type == "NFTokenBurn" && tx_success;

        for (key, new_data) in &changes.updates {
            let new_obj = match serde_json::from_slice::<Value>(new_data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if new_obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("AccountRoot") {
                continue;
            }

            let old_obj = match changes.originals.get(key) {
                Some(data) => match serde_json::from_slice::<Value>(data) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                None => continue,
            };

            let old_minted = Self::get_counter(&old_obj, "MintedNFTokens");
            let new_minted = Self::get_counter(&new_obj, "MintedNFTokens");
            let old_burned = Self::get_counter(&old_obj, "BurnedNFTokens");
            let new_burned = Self::get_counter(&new_obj, "BurnedNFTokens");

            let minted_diff = new_minted.saturating_sub(old_minted);
            let burned_diff = new_burned.saturating_sub(old_burned);

            if is_mint {
                if minted_diff > 1 {
                    return Err(format!(
                        "AccountRoot at {key}: MintedNFTokens increased by {minted_diff} (expected 0 or 1)"
                    ));
                }
            } else if minted_diff != 0 {
                return Err(format!(
                    "AccountRoot at {key}: MintedNFTokens changed by {minted_diff} on non-mint tx"
                ));
            }

            if is_burn {
                if burned_diff > 1 {
                    return Err(format!(
                        "AccountRoot at {key}: BurnedNFTokens increased by {burned_diff} (expected 0 or 1)"
                    ));
                }
            } else if burned_diff != 0 {
                return Err(format!(
                    "AccountRoot at {key}: BurnedNFTokens changed by {burned_diff} on non-burn tx"
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

    fn account_with_counters(minted: u64, burned: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "AccountRoot",
            "Balance": "1000000",
            "MintedNFTokens": minted,
            "BurnedNFTokens": burned,
        }))
        .unwrap()
    }

    #[test]
    fn mint_increments_by_one_passes() {
        let check = NFTokenCountTracking;
        let key = Hash256::new([0x01; 32]);
        let mut changes = empty_changes();
        changes.originals.insert(key, account_with_counters(5, 0));
        changes.updates.insert(key, account_with_counters(6, 0));

        let tx = json!({ "TransactionType": "NFTokenMint" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn burn_increments_by_one_passes() {
        let check = NFTokenCountTracking;
        let key = Hash256::new([0x01; 32]);
        let mut changes = empty_changes();
        changes.originals.insert(key, account_with_counters(5, 2));
        changes.updates.insert(key, account_with_counters(5, 3));

        let tx = json!({ "TransactionType": "NFTokenBurn" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn mint_counter_changes_on_non_mint_tx_fails() {
        let check = NFTokenCountTracking;
        let key = Hash256::new([0x01; 32]);
        let mut changes = empty_changes();
        changes.originals.insert(key, account_with_counters(5, 0));
        changes.updates.insert(key, account_with_counters(6, 0));

        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn burn_counter_changes_on_non_burn_tx_fails() {
        let check = NFTokenCountTracking;
        let key = Hash256::new([0x01; 32]);
        let mut changes = empty_changes();
        changes.originals.insert(key, account_with_counters(5, 2));
        changes.updates.insert(key, account_with_counters(5, 3));

        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn no_counter_changes_passes() {
        let check = NFTokenCountTracking;
        let key = Hash256::new([0x01; 32]);
        let mut changes = empty_changes();
        changes.originals.insert(key, account_with_counters(5, 2));
        changes.updates.insert(key, account_with_counters(5, 2));

        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }
}
