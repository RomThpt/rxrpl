use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct DepositPreauth {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub authorize: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
}

impl LedgerObject for DepositPreauth {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::DepositPreauth
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
