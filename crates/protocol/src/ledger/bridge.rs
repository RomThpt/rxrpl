use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Bridge {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    pub xchain_bridge: Value,
    pub signature_reward: Value,
    #[serde(rename = "XChainClaimID")]
    pub xchain_claim_id: String,
    pub xchain_account_create_count: String,
    pub xchain_account_claim_count: String,
    pub owner_node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_account_create_amount: Option<Value>,
}

impl LedgerObject for Bridge {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Bridge
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
