use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct XChainOwnedClaimId {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub account: String,
    #[serde(rename = "XChainBridge")]
    pub xchain_bridge: Value,
    #[serde(rename = "XChainClaimID")]
    pub xchain_claim_id: String,
    pub other_chain_source: String,
    #[serde(rename = "XChainClaimAttestations")]
    pub xchain_claim_attestations: Vec<Value>,
    pub signature_reward: Value,
    pub owner_node: String,
}

impl LedgerObject for XChainOwnedClaimId {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::XChainOwnedClaimId
    }

    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}
