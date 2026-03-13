use serde::{Deserialize, Serialize};

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MpTokenIssuance {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub issuer: String,
    pub sequence: u32,
    pub owner_node: String,
    pub outstanding_amount: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer_fee: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_scale: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_amount: Option<String>,
    #[serde(rename = "MPTokenMetadata")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mptoken_metadata: Option<String>,
    #[serde(rename = "DomainID")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain_id: Option<String>,
}

impl LedgerObject for MpTokenIssuance {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::MPTokenIssuance
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
