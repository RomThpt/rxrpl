use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct XChainOwnedCreateAccountClaimId {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub xchain_bridge: Value,
    pub xchain_account_create_count: String,
    pub xchain_create_account_attestations: Vec<Value>,
    pub owner_node: String,
}

impl LedgerObject for XChainOwnedCreateAccountClaimId {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::XChainOwnedCreateAccountClaimId
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
