use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// AMM transaction types that this invariant applies to.
const AMM_TX_TYPES: &[&str] = &[
    "AMMCreate",
    "AMMDeposit",
    "AMMWithdraw",
    "AMMClawback",
    "AMMBid",
    "AMMVote",
    "AMMDelete",
];

/// Invariant: AMM constant product formula must hold.
///
/// After any AMM operation, `sqrt(asset1 * asset2) >= lp_tokens` must be
/// satisfied. This ensures the AMM pool was not drained below its invariant.
pub struct ValidAmm;

impl ValidAmm {
    fn parse_amount(val: &Value) -> Option<f64> {
        // XRP drops as string
        if let Some(s) = val.as_str() {
            return s.parse::<f64>().ok();
        }
        // IOU amount object
        if let Some(s) = val.get("value").and_then(|v| v.as_str()) {
            return s.parse::<f64>().ok();
        }
        None
    }
}

impl InvariantCheck for ValidAmm {
    fn name(&self) -> &str {
        "ValidAmm"
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

        if !AMM_TX_TYPES.contains(&tx_type) {
            return Ok(());
        }

        for (key, data) in changes.updates.iter().chain(changes.inserts.iter()) {
            let obj = match serde_json::from_slice::<Value>(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("AMM") {
                continue;
            }

            let asset1 = match obj.get("Asset1Amount").and_then(|v| Self::parse_amount(v)) {
                Some(v) if v >= 0.0 => v,
                Some(v) => {
                    return Err(format!("AMM at {key} has negative Asset1Amount: {v}"));
                }
                None => continue,
            };

            let asset2 = match obj.get("Asset2Amount").and_then(|v| Self::parse_amount(v)) {
                Some(v) if v >= 0.0 => v,
                Some(v) => {
                    return Err(format!("AMM at {key} has negative Asset2Amount: {v}"));
                }
                None => continue,
            };

            let lp_tokens = match obj
                .get("LPTokenBalance")
                .and_then(|v| Self::parse_amount(v))
            {
                Some(v) if v >= 0.0 => v,
                Some(v) => {
                    return Err(format!("AMM at {key} has negative LPTokenBalance: {v}"));
                }
                None => continue,
            };

            // AMMDelete can drain the pool to zero
            if tx_type == "AMMDelete" && asset1 == 0.0 && asset2 == 0.0 && lp_tokens == 0.0 {
                continue;
            }

            let geometric_mean = (asset1 * asset2).sqrt();
            if geometric_mean + f64::EPSILON < lp_tokens {
                return Err(format!(
                    "AMM at {key}: sqrt({asset1} * {asset2}) = {geometric_mean} < LPTokens {lp_tokens}"
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

    fn amm_entry(asset1: &str, asset2: &str, lp: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "AMM",
            "Asset1Amount": asset1,
            "Asset2Amount": { "currency": "USD", "issuer": "rX", "value": asset2 },
            "LPTokenBalance": { "currency": "LPT", "issuer": "rAmm", "value": lp },
        }))
        .unwrap()
    }

    #[test]
    fn valid_amm_passes() {
        let check = ValidAmm;
        let mut changes = empty_changes();
        // sqrt(1000000 * 100) = sqrt(100000000) = 10000, lp = 9000
        changes.updates.insert(
            Hash256::new([0x01; 32]),
            amm_entry("1000000", "100", "9000"),
        );
        let tx = json!({ "TransactionType": "AMMDeposit" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn lp_exceeds_geometric_mean_fails() {
        let check = ValidAmm;
        let mut changes = empty_changes();
        // sqrt(100 * 100) = 100, lp = 200 (violation)
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), amm_entry("100", "100", "200"));
        let tx = json!({ "TransactionType": "AMMDeposit" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }

    #[test]
    fn non_amm_tx_ignored() {
        let check = ValidAmm;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), amm_entry("100", "100", "200"));
        let tx = json!({ "TransactionType": "Payment" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn amm_delete_zeroed_passes() {
        let check = ValidAmm;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), amm_entry("0", "0", "0"));
        let tx = json!({ "TransactionType": "AMMDelete" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_ok());
    }

    #[test]
    fn negative_asset_fails() {
        let check = ValidAmm;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), amm_entry("-100", "100", "50"));
        let tx = json!({ "TransactionType": "AMMDeposit" });
        assert!(check.check(&changes, 100, 100, Some(&tx)).is_err());
    }
}
