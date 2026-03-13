use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultWithdraw transaction withdraws assets from a vault.
    VaultWithdraw => TransactionType::VaultWithdraw,
    {
        "VaultID" => vault_id: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Destination" => destination: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DestinationTag" => destination_tag: Option<u32>
    }
}
