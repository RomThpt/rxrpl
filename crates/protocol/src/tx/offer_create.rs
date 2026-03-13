use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An OfferCreate transaction places an order on the DEX.
    OfferCreate => TransactionType::OfferCreate,
    {
        "TakerGets" => taker_gets: serde_json::Value,
        "TakerPays" => taker_pays: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Expiration" => expiration: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "OfferSequence" => offer_sequence: Option<u32>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "OfferCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "TakerGets": "1000000",
            "TakerPays": {
                "value": "1",
                "currency": "USD",
                "issuer": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
            }
        });
        let tx = OfferCreate::from_json(&json).unwrap();
        assert_eq!(tx.taker_gets, serde_json::Value::String("1000000".into()));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "OfferCreate");
    }
}
