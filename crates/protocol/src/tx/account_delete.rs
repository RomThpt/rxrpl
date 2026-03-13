use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AccountDelete transaction deletes an account and sends remaining XRP to a destination.
    AccountDelete => TransactionType::AccountDelete,
    {
        "Destination" => destination: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DestinationTag" => destination_tag: Option<u32>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AccountDelete",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "2000000",
            "Destination": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
        });
        let tx = AccountDelete::from_json(&json).unwrap();
        assert_eq!(tx.destination, "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy");
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AccountDelete");
    }
}
