use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: Offer entries must have positive TakerPays and TakerGets amounts.
///
/// An Offer with zero or negative amounts is invalid and indicates a
/// transactor bug. Both XRP (string) and IOU (object) amounts are checked.
pub struct NoBadOffers;

impl NoBadOffers {
    fn validate_amount(
        amount: &serde_json::Value,
        field: &str,
        key: &impl std::fmt::Display,
    ) -> Result<(), String> {
        if let Some(s) = amount.as_str() {
            // XRP drops as string
            let drops: u64 = s
                .parse()
                .map_err(|_| format!("Offer at {key} has invalid {field}: {s}"))?;
            if drops == 0 {
                return Err(format!("Offer at {key} has zero {field}"));
            }
        } else if amount.is_object() {
            // IOU amount object
            let value_str = amount
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("Offer at {key} has {field} missing value field"))?;
            let value: f64 = value_str
                .parse()
                .map_err(|_| format!("Offer at {key} has invalid {field}.value: {value_str}"))?;
            if value <= 0.0 {
                return Err(format!(
                    "Offer at {key} has non-positive {field}.value: {value_str}"
                ));
            }
        } else {
            return Err(format!("Offer at {key} has unexpected {field} type"));
        }
        Ok(())
    }
}

impl InvariantCheck for NoBadOffers {
    fn name(&self) -> &str {
        "NoBadOffers"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.inserts.iter().chain(changes.updates.iter()) {
            if let Ok(obj) = serde_json::from_slice::<serde_json::Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Offer") {
                    continue;
                }

                let taker_pays = obj
                    .get("TakerPays")
                    .ok_or_else(|| format!("Offer at {key} missing TakerPays"))?;
                Self::validate_amount(taker_pays, "TakerPays", key)?;

                let taker_gets = obj
                    .get("TakerGets")
                    .ok_or_else(|| format!("Offer at {key} missing TakerGets"))?;
                Self::validate_amount(taker_gets, "TakerGets", key)?;
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
    fn valid_offer_passes() {
        let check = NoBadOffers;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "TakerPays": "1000000",
            "TakerGets": { "currency": "USD", "issuer": "rIssuer", "value": "100" },
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn zero_taker_pays_fails() {
        let check = NoBadOffers;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "TakerPays": "0",
            "TakerGets": "1000000",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn negative_taker_gets_fails() {
        let check = NoBadOffers;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "TakerPays": "1000000",
            "TakerGets": { "currency": "USD", "issuer": "rIssuer", "value": "-50" },
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn missing_taker_pays_fails() {
        let check = NoBadOffers;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Offer",
            "TakerGets": "1000000",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_offer_ignored() {
        let check = NoBadOffers;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
