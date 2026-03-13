use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CredentialAccept transaction accepts a credential issued to the account.
    CredentialAccept => TransactionType::CredentialAccept,
    {
        "Issuer" => issuer: String,
        "CredentialType" => credential_type: String
    }
}
