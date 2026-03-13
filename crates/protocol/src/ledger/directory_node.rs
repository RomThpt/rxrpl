use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DirectoryNode {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taker_pays_currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taker_pays_issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taker_gets_currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taker_gets_issuer: Option<String>,
    pub root_index: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_next: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_previous: Option<u64>,
}

impl LedgerObject for DirectoryNode {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::DirectoryNode
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
