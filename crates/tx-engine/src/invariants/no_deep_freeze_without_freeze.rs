use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

// RippleState flags (from rippled source).
const LSF_LOW_FREEZE: u64 = 0x0040_0000;
const LSF_HIGH_FREEZE: u64 = 0x0080_0000;
const LSF_LOW_DEEP_FREEZE: u64 = 0x1000_0000;
const LSF_HIGH_DEEP_FREEZE: u64 = 0x2000_0000;

/// Invariant: DeepFreeze requires Freeze.
///
/// If `lsfLowDeepFreeze` is set on a RippleState, `lsfLowFreeze` must also
/// be set (and likewise for the High side). A deep-frozen trust line that is
/// not frozen is an invalid state.
pub struct NoDeepFreezeWithoutFreeze;

impl InvariantCheck for NoDeepFreezeWithoutFreeze {
    fn name(&self) -> &str {
        "NoDeepFreezeWithoutFreeze"
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
                if obj.get("LedgerEntryType").and_then(|v| v.as_str()) != Some("RippleState") {
                    continue;
                }

                let flags = obj.get("Flags").and_then(|v| v.as_u64()).unwrap_or(0);

                if (flags & LSF_LOW_DEEP_FREEZE) != 0 && (flags & LSF_LOW_FREEZE) == 0 {
                    return Err(format!(
                        "RippleState at {key} has lsfLowDeepFreeze without lsfLowFreeze"
                    ));
                }

                if (flags & LSF_HIGH_DEEP_FREEZE) != 0 && (flags & LSF_HIGH_FREEZE) == 0 {
                    return Err(format!(
                        "RippleState at {key} has lsfHighDeepFreeze without lsfHighFreeze"
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

    fn ripple_state_flags(flags: u64) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "RippleState",
            "Flags": flags,
            "Balance": { "currency": "USD", "issuer": "rX", "value": "100" },
            "LowLimit": { "currency": "USD", "issuer": "rA", "value": "1000" },
            "HighLimit": { "currency": "USD", "issuer": "rB", "value": "1000" },
        }))
        .unwrap()
    }

    #[test]
    fn no_freeze_flags_passes() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), ripple_state_flags(0));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn low_deep_freeze_with_low_freeze_passes() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            ripple_state_flags(LSF_LOW_FREEZE | LSF_LOW_DEEP_FREEZE),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn high_deep_freeze_with_high_freeze_passes() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            ripple_state_flags(LSF_HIGH_FREEZE | LSF_HIGH_DEEP_FREEZE),
        );
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn low_deep_freeze_without_low_freeze_fails() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            ripple_state_flags(LSF_LOW_DEEP_FREEZE),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn high_deep_freeze_without_high_freeze_fails() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        changes.inserts.insert(
            Hash256::new([0x01; 32]),
            ripple_state_flags(LSF_HIGH_DEEP_FREEZE),
        );
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn non_ripple_state_ignored() {
        let check = NoDeepFreezeWithoutFreeze;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Flags": LSF_LOW_DEEP_FREEZE,
        }))
        .unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }
}
