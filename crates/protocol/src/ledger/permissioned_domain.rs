use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PermissionedDomain {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub owner: String,
    pub sequence: u32,
    pub accepted_credentials: Vec<Value>,
    pub owner_node: String,
}

impl LedgerObject for PermissionedDomain {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::PermissionedDomain
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
