use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CredentialCreate transaction creates a verifiable credential.
    CredentialCreate => TransactionType::CredentialCreate,
    {
        "Subject" => subject: String,
        "CredentialType" => credential_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Expiration" => expiration: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "URI" => uri: Option<String>
    }
}
