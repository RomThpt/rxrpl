use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An EscrowCancel transaction returns escrowed XRP to the sender.
    EscrowCancel => TransactionType::EscrowCancel,
    {
        "Owner" => owner: String,
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
            "TransactionType": "EscrowCancel",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "OfferSequence": 7
        });
        let tx = EscrowCancel::from_json(&json).unwrap();
        assert_eq!(tx.offer_sequence, 7);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "EscrowCancel");
    }
}
