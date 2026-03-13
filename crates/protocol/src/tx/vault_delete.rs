use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultDelete transaction deletes an existing vault.
    VaultDelete => TransactionType::VaultDelete,
    {
        "VaultID" => vault_id: String
    }
}
