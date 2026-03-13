use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CheckCreate transaction creates a Check object.
    CheckCreate => TransactionType::CheckCreate,
    {
        "Destination" => destination: String,
        "SendMax" => send_max: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DestinationTag" => destination_tag: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Expiration" => expiration: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "InvoiceID" => invoice_id: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "CheckCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Destination": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            "SendMax": "1000000"
        });
        let tx = CheckCreate::from_json(&json).unwrap();
        assert_eq!(tx.destination, "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy");
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "CheckCreate");
    }
}
