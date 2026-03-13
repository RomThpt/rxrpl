use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultClawback transaction claws back assets from a vault holder.
    VaultClawback => TransactionType::VaultClawback,
    {
        "VaultID" => vault_id: String,
        "Holder" => holder: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>
    }
}
