use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An MPTokenIssuanceDestroy transaction destroys an existing multi-purpose token issuance.
    MPTokenIssuanceDestroy => TransactionType::MPTokenIssuanceDestroy,
    {
        "MPTokenIssuanceID" => mptoken_issuance_id: String
    }
}
