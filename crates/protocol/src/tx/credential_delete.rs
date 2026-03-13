use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CredentialDelete transaction deletes a credential.
    CredentialDelete => TransactionType::CredentialDelete,
    {
        "CredentialType" => credential_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Subject" => subject: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Issuer" => issuer: Option<String>
    }
}
