use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Delete a Loan object.
    LoanDelete => TransactionType::LoanDelete,
    {
        "LoanID" => loan_id: String
    }
}
