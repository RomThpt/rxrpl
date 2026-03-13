use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultSet transaction modifies properties of an existing vault.
    VaultSet => TransactionType::VaultSet,
    {
        "VaultID" => vault_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "AssetsMaximum" => assets_maximum: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DomainID" => domain_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Data" => data: Option<String>
    }
}
