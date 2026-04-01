use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// Create or update a LoanBroker object linked to a Vault.
    LoanBrokerSet => TransactionType::LoanBrokerSet,
    {
        "VaultID" => vault_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "ManagementFeeRate" => management_fee_rate: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DebtMaximum" => debt_maximum: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "CoverRateMinimum" => cover_rate_minimum: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "CoverRateLiquidation" => cover_rate_liquidation: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Data" => data: Option<String>
    }
}
