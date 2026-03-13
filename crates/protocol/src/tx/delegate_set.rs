use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A DelegateSet transaction sets or removes delegation permissions for an account.
    DelegateSet => TransactionType::DelegateSet,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "Authorize" => authorize: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Permissions" => permissions: Option<Vec<serde_json::Value>>
    }
}
