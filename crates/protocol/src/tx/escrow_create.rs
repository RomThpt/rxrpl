use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An EscrowCreate transaction sequesters XRP until certain conditions are met.
    EscrowCreate => TransactionType::EscrowCreate,
    {
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "CancelAfter" => cancel_after: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "FinishAfter" => finish_after: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Condition" => condition: Option<String>,
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
            "TransactionType": "EscrowCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Destination": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            "Amount": "1000000",
            "FinishAfter": 533257958,
            "CancelAfter": 533344358
        });
        let tx = EscrowCreate::from_json(&json).unwrap();
        assert_eq!(tx.finish_after, Some(533257958));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "EscrowCreate");
    }
}
