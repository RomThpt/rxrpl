use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Create or update a Loan object.
    LoanSet => TransactionType::LoanSet,
    {
        "LoanBrokerID" => loan_broker_id: String,
        "Borrower" => borrower: String,
        "LoanMaturityDate" => loan_maturity_date: u32,
        "LoanRate" => loan_rate: u16,
        "LoanPrincipal" => loan_principal: serde_json::Value,
        "CounterpartySignature" => counterparty_signature: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LoanRateLate" => loan_rate_late: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LoanRateFull" => loan_rate_full: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LoanOriginationFeeRate" => loan_origination_fee_rate: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LoanServiceFeeRate" => loan_service_fee_rate: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LateFeeRate" => late_fee_rate: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "EarlyFeeRate" => early_fee_rate: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "GracePeriodDays" => grace_period_days: Option<u16>
    }
}
