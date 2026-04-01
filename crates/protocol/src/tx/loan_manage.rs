use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Manage a Loan (e.g. activate, freeze, liquidate).
    LoanManage => TransactionType::LoanManage,
    {
        "LoanID" => loan_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Action" => action: Option<u8>
    }
}
