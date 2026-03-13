use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct LedgerHashes {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_ledger_sequence: Option<u32>,
}

impl LedgerObject for LedgerHashes {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::LedgerHashes
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
