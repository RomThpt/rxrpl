use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

/// Ledger entry representing a single hook state key-value pair.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HookState {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    /// Account that owns this state entry.
    pub account: String,
    /// Hex-encoded state key.
    pub hook_state_key: String,
    /// Hex-encoded state data.
    pub hook_state_data: String,
    /// Namespace this state entry belongs to (hex-encoded 32 bytes).
    pub hook_state_namespace: String,
}

impl LedgerObject for HookState {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::HookState
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_hook_state() {
        let json = serde_json::json!({
            "LedgerEntryType": "HookState",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "HookStateKey": "A".repeat(64),
            "HookStateData": "DEADBEEF",
            "HookStateNamespace": "B".repeat(64),
            "Flags": 0
        });
        let hs = HookState::from_json(&json).unwrap();
        assert_eq!(hs.account, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
        assert_eq!(hs.hook_state_data, "DEADBEEF");
    }
}
