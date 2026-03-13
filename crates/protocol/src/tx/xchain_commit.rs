use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainCommit transaction commits funds to a cross-chain transfer.
    XChainCommit => TransactionType::XChainCommit,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "XChainClaimID" => xchain_claim_id: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "OtherChainDestination" => other_chain_destination: Option<String>
    }
}
