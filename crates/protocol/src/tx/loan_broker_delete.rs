use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Delete a LoanBroker object.
    LoanBrokerDelete => TransactionType::LoanBrokerDelete,
    {
        "LoanBrokerID" => loan_broker_id: String
    }
}
