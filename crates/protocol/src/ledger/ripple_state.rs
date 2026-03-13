use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RippleState {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub balance: serde_json::Value,
    pub low_limit: serde_json::Value,
    pub high_limit: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_quality_in: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub low_quality_out: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_quality_in: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub high_quality_out: Option<u32>,
}

impl LedgerObject for RippleState {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::RippleState
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
