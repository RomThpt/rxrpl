use serde::{Deserialize, Serialize};

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MpToken {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    #[serde(rename = "MPTokenIssuanceID")]
    pub mptoken_issuance_id: String,
    pub owner_node: String,
    #[serde(rename = "MPTAmount")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpt_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_amount: Option<String>,
}

impl LedgerObject for MpToken {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::MPToken
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
