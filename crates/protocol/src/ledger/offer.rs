use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Offer {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub sequence: u32,
    pub taker_pays: serde_json::Value,
    pub taker_gets: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub book_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u32>,
}

impl LedgerObject for Offer {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Offer
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
