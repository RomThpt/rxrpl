use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Amendments {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendments: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub majorities: Option<Vec<serde_json::Value>>,
}

impl LedgerObject for Amendments {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Amendments
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
