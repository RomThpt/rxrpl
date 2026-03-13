use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Check {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub destination: String,
    pub send_max: serde_json::Value,
    pub sequence: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoice_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tag: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_tag: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
}

impl LedgerObject for Check {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Check
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
