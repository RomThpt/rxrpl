use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A Payment transaction sends value from one account to another.
    Payment => TransactionType::Payment,
    {
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DestinationTag" => destination_tag: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "InvoiceID" => invoice_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Paths" => paths: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "SendMax" => send_max: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DeliverMin" => deliver_min: Option<serde_json::Value>
    }
}

impl Payment {
    /// Create an XRP-to-XRP payment (amount in drops).
    pub fn xrp(
        account: impl Into<String>,
        destination: impl Into<String>,
        drops: u64,
        fee: impl Into<String>,
    ) -> Self {
        Self {
            common: crate::tx::common::CommonFields::new(account, fee),
            destination: destination.into(),
            amount: serde_json::Value::String(drops.to_string()),
            destination_tag: None,
            invoice_id: None,
            paths: None,
            send_max: None,
            deliver_min: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn xrp_payment() {
        let tx = Payment::xrp(
            "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            1_000_000,
            "12",
        );
        assert_eq!(Payment::transaction_type(), TransactionType::Payment);
        assert_eq!(tx.common().account, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
        assert_eq!(tx.amount, serde_json::Value::String("1000000".to_string()));
    }

    #[test]
    fn serde_roundtrip() {
        let tx = Payment::xrp(
            "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            1_000_000,
            "12",
        );
        let json = tx.to_json().unwrap();
        assert_eq!(json["TransactionType"], "Payment");
        assert_eq!(json["Account"], "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
        assert_eq!(json["Destination"], "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy");
        assert_eq!(json["Amount"], "1000000");
        assert_eq!(json["Fee"], "12");

        let decoded = Payment::from_json(&json).unwrap();
        assert_eq!(decoded.destination, tx.destination);
    }

    #[test]
    fn iou_payment_json() {
        let json = serde_json::json!({
            "TransactionType": "Payment",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Destination": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            "Amount": {
                "value": "100",
                "currency": "USD",
                "issuer": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk"
            },
            "Fee": "12"
        });
        let tx = Payment::from_json(&json).unwrap();
        assert!(tx.amount.is_object());
    }
}
