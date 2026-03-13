use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Ticket {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub ticket_sequence: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
}

impl LedgerObject for Ticket {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Ticket
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
