use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct SignerList {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub signer_quorum: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_entries: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signer_list_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
}

impl LedgerObject for SignerList {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::SignerList
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
