use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainClaim transaction claims funds from a cross-chain transfer.
    XChainClaim => TransactionType::XChainClaim,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "XChainClaimID" => xchain_claim_id: String,
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DestinationTag" => destination_tag: Option<u32>
    }
}
