use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMClawback transaction claws back tokens from an AMM pool.
    AMMClawback => TransactionType::AMMClawback,
    {
        "Holder" => holder: String,
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>
    }
}
