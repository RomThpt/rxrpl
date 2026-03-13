use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NFTokenOffer {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub owner: String,
    #[serde(rename = "NFTokenID")]
    pub nftoken_id: String,
    pub amount: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "NFTokenOfferNode")]
    pub nftoken_offer_node: Option<String>,
}

impl LedgerObject for NFTokenOffer {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::NFTokenOffer
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
