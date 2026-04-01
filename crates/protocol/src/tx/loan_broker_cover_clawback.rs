use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Clawback cover funds from a LoanBroker.
    LoanBrokerCoverClawback => TransactionType::LoanBrokerCoverClawback,
    {
        "LoanBrokerID" => loan_broker_id: String
    }
}
