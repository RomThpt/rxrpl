use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A BatchSubmit transaction submits a batch of transactions atomically.
    BatchSubmit => TransactionType::BatchSubmit,
    {
        "RawTransactions" => raw_transactions: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "BatchSigners" => batch_signers: Option<Vec<serde_json::Value>>
    }
}
