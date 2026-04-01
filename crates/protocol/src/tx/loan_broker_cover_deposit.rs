use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Deposit cover funds into a LoanBroker.
    LoanBrokerCoverDeposit => TransactionType::LoanBrokerCoverDeposit,
    {
        "LoanBrokerID" => loan_broker_id: String,
        "Amount" => amount: serde_json::Value
    }
}
