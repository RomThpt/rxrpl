use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An MPTokenAuthorize transaction authorizes an account to hold a multi-purpose token.
    MPTokenAuthorize => TransactionType::MPTokenAuthorize,
    {
        "MPTokenIssuanceID" => mptoken_issuance_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Holder" => holder: Option<String>
    }
}
