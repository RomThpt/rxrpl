use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An OfferCancel transaction removes an offer from the DEX.
    OfferCancel => TransactionType::OfferCancel,
    {
        "OfferSequence" => offer_sequence: u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "OfferCancel",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "OfferSequence": 7
        });
        let tx = OfferCancel::from_json(&json).unwrap();
        assert_eq!(tx.offer_sequence, 7);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["OfferSequence"], 7);
    }
}
