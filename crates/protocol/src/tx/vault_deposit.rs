use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultDeposit transaction deposits assets into a vault.
    VaultDeposit => TransactionType::VaultDeposit,
    {
        "VaultID" => vault_id: String,
        "Amount" => amount: serde_json::Value
    }
}
