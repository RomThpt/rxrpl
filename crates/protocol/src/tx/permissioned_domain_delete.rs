use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A PermissionedDomainDelete transaction deletes a permissioned domain.
    PermissionedDomainDelete => TransactionType::PermissionedDomainDelete,
    {
        "DomainID" => domain_id: String
    }
}
