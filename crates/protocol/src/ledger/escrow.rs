use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Escrow {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub destination: String,
    pub amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_after: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_after: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_tag: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_tag: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_node: Option<String>,
}

impl LedgerObject for Escrow {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Escrow
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
