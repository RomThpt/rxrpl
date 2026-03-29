use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

/// A single hook entry within the Hooks array on a HookDefinition.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HookEntry {
    /// Hex-encoded WASM bytecode for the hook.
    pub create_code: String,
    /// SHA-256 hash of the WASM bytecode.
    pub hook_hash: String,
    /// Namespace for hook state isolation (hex-encoded 32 bytes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_namespace: Option<String>,
    /// Bit flags controlling when the hook fires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_on: Option<String>,
    /// API version of the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_api_version: Option<u16>,
    /// Hook parameters as key-value pairs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_parameters: Option<Vec<serde_json::Value>>,
    /// Grant authorizations for other accounts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_grants: Option<Vec<serde_json::Value>>,
}

/// Ledger entry representing a hook definition attached to an account.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct HookDefinition {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    /// Account that owns this hook definition.
    pub account: String,
    /// The hooks installed on this account (up to 10).
    pub hooks: Vec<HookEntry>,
    /// Reference count for shared hook code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_count: Option<u64>,
}

impl LedgerObject for HookDefinition {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::HookDefinition
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_hook_definition() {
        let json = serde_json::json!({
            "LedgerEntryType": "HookDefinition",
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Hooks": [
                {
                    "CreateCode": "0061736d",
                    "HookHash": "A".repeat(64),
                    "HookNamespace": "B".repeat(64),
                    "HookOn": "0000000000000000",
                }
            ],
            "Flags": 0
        });
        let hd = HookDefinition::from_json(&json).unwrap();
        assert_eq!(hd.account, "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh");
        assert_eq!(hd.hooks.len(), 1);
        assert_eq!(hd.hooks[0].create_code, "0061736d");
    }
}
