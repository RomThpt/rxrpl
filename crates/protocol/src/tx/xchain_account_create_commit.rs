use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainAccountCreateCommit transaction creates a new account on
    /// the destination chain as part of a cross-chain transfer.
    XChainAccountCreateCommit => TransactionType::XChainAccountCreateCommit,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        "SignatureReward" => signature_reward: serde_json::Value
    }
}
