use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Vault {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub owner: String,
    pub account: String,
    pub sequence: u32,
    pub owner_node: String,
    pub asset: Value,
    pub withdrawal_policy: u8,
    #[serde(rename = "ShareMPTID")]
    pub share_mpt_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets_total: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets_available: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assets_maximum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loss_unrealized: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

impl LedgerObject for Vault {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Vault
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
