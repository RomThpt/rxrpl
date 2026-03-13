use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A VaultCreate transaction creates a new single-asset vault.
    VaultCreate => TransactionType::VaultCreate,
    {
        "Asset" => asset: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "AssetsMaximum" => assets_maximum: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MPTokenMetadata" => mptoken_metadata: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DomainID" => domain_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "WithdrawalPolicy" => withdrawal_policy: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Data" => data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Scale" => scale: Option<u8>
    }
}
