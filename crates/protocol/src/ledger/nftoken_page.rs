use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NFTokenPage {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "NFTokens")]
    pub nftokens: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_min: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_page_min: Option<String>,
}

impl LedgerObject for NFTokenPage {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::NFTokenPage
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
