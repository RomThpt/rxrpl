use serde::{Deserialize, Serialize};

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Credential {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub subject: String,
    pub issuer: String,
    pub credential_type: String,
    pub issuer_node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<u32>,
    #[serde(rename = "URI")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl LedgerObject for Credential {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Credential
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
