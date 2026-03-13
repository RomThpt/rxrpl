use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainCreateClaimId transaction creates a new cross-chain claim ID.
    XChainCreateClaimId => TransactionType::XChainCreateClaimId,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "SignatureReward" => signature_reward: serde_json::Value,
        "OtherChainSource" => other_chain_source: String
    }
}
