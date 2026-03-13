use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An NFTokenBurn transaction destroys an NFToken.
    NFTokenBurn => TransactionType::NFTokenBurn,
    {
        "NFTokenID" => nftoken_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Owner" => owner: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "NFTokenBurn",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "NFTokenID": "000800006203F49C21D5D6E022CB16DE3538F248662FC73C00000000000000000000000000000001"
        });
        let tx = NFTokenBurn::from_json(&json).unwrap();
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "NFTokenBurn");
    }
}
