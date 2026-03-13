use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An MPTokenIssuanceSet transaction modifies properties of a multi-purpose token issuance.
    MPTokenIssuanceSet => TransactionType::MPTokenIssuanceSet,
    {
        "MPTokenIssuanceID" => mptoken_issuance_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Holder" => holder: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DomainID" => domain_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "MPTokenMetadata" => mptoken_metadata: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "TransferFee" => transfer_fee: Option<u16>
    }
}
