use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainCreateBridge transaction creates a new cross-chain bridge.
    XChainCreateBridge => TransactionType::XChainCreateBridge,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        "SignatureReward" => signature_reward: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MinAccountCreateAmount" => min_account_create_amount: Option<serde_json::Value>
    }
}
