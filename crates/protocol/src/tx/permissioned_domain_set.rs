use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A PermissionedDomainSet transaction creates or modifies a permissioned domain.
    PermissionedDomainSet => TransactionType::PermissionedDomainSet,
    {
        "AcceptedCredentials" => accepted_credentials: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DomainID" => domain_id: Option<String>
    }
}
