use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NegativeUnl {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_validators: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_to_disable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validator_to_re_enable: Option<String>,
}

impl LedgerObject for NegativeUnl {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::NegativeUNL
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
