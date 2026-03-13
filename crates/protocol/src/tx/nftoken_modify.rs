use super::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An NFTokenModify transaction modifies an existing NFToken.
    NFTokenModify => TransactionType::NFTokenModify,
    {
        "NFTokenID" => nftoken_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Owner" => owner: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "URI" => uri: Option<String>
    }
}
