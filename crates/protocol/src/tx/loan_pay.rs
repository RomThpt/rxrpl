use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Make a payment toward a Loan.
    LoanPay => TransactionType::LoanPay,
    {
        "LoanID" => loan_id: String,
        "PaymentAmount" => payment_amount: serde_json::Value
    }
}
