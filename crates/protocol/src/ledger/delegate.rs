use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Delegate {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub authorize: String,
    pub permissions: Vec<Value>,
    pub owner_node: String,
}

impl LedgerObject for Delegate {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Delegate
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
