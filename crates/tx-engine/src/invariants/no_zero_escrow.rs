use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Maximum XRP supply in drops.
const MAX_XRP_DROPS: u64 = 100_000_000_000_000_000;

/// Invariant: Escrow entries must have a positive amount within valid range.
///
/// Every Escrow created or modified must have Amount > 0 and Amount <= MAX_XRP_DROPS.
pub struct NoZeroEscrow;

impl InvariantCheck for NoZeroEscrow {
    fn name(&self) -> &str {
        "NoZeroEscrow"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.inserts.iter().chain(changes.updates.iter()) {
            if let Ok(obj) = serde_json::from_slice::<Value>(data) {
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("Escrow") {
                    continue;
                }

                let amount_str = obj
                    .get("Amount")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("Escrow at {key} missing Amount"))?;

                let drops: u64 = amount_str
                    .parse()
                    .map_err(|_| format!("Escrow at {key} has unparseable Amount: {amount_str}"))?;

                if drops == 0 {
                    return Err(format!("Escrow at {key} has zero Amount"));
                }

                if drops > MAX_XRP_DROPS {
                    return Err(format!(
                        "Escrow at {key} has Amount {drops} exceeding max {MAX_XRP_DROPS}"
                    ));
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

    fn escrow_bytes(amount: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "Escrow",
            "Amount": amount,
        }))
        .unwrap()
    }

    #[test]
    fn valid_escrow_passes() {
        let check = NoZeroEscrow;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), escrow_bytes("1000000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn zero_amount_fails() {
        let check = NoZeroEscrow;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), escrow_bytes("0"));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn exceeds_max_fails() {
        let check = NoZeroEscrow;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            escrow_bytes(&(MAX_XRP_DROPS + 1).to_string()),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn max_amount_passes() {
        let check = NoZeroEscrow;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            escrow_bytes(&MAX_XRP_DROPS.to_string()),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn non_escrow_ignored() {
        let check = NoZeroEscrow;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Amount": "0",
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
