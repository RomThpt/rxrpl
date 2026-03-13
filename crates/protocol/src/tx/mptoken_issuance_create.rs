use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An MPTokenIssuanceCreate transaction creates a new multi-purpose token issuance.
    MPTokenIssuanceCreate => TransactionType::MPTokenIssuanceCreate,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "AssetScale" => asset_scale: Option<u8>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "TransferFee" => transfer_fee: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MaximumAmount" => maximum_amount: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MPTokenMetadata" => mptoken_metadata: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DomainID" => domain_id: Option<String>
    }
}
