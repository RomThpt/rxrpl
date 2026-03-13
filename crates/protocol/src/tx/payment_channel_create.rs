use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A PaymentChannelCreate transaction creates a new payment channel.
    PaymentChannelCreate => TransactionType::PaymentChannelCreate,
    {
        "Destination" => destination: String,
        "Amount" => amount: serde_json::Value,
        "SettleDelay" => settle_delay: u32,
        "PublicKey" => public_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "CancelAfter" => cancel_after: Option<u32>,
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
            "TransactionType": "PaymentChannelCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Destination": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            "Amount": "1000000",
            "SettleDelay": 86400,
            "PublicKey": "023693F15967AE357D0327974AD46FE3C127113B1110D6044010006ECB8B8768ED"
        });
        let tx = PaymentChannelCreate::from_json(&json).unwrap();
        assert_eq!(tx.settle_delay, 86400);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "PaymentChannelCreate");
    }
}
