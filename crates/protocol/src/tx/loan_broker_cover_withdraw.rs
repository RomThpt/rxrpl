use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Withdraw cover funds from a LoanBroker.
    LoanBrokerCoverWithdraw => TransactionType::LoanBrokerCoverWithdraw,
    {
        "LoanBrokerID" => loan_broker_id: String,
        "Amount" => amount: serde_json::Value
    }
}
