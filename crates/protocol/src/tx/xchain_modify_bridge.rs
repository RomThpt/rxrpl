use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An XChainModifyBridge transaction modifies an existing cross-chain bridge.
    XChainModifyBridge => TransactionType::XChainModifyBridge,
    {
        "XChainBridge" => xchain_bridge: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "SignatureReward" => signature_reward: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MinAccountCreateAmount" => min_account_create_amount: Option<serde_json::Value>
    }
}
