use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct FeeSettings {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_fee: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_fee_units: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_base: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_increment: Option<u32>,
}

impl LedgerObject for FeeSettings {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::FeeSettings
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
