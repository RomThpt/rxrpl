use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An EscrowFinish transaction delivers XRP from a held escrow.
    EscrowFinish => TransactionType::EscrowFinish,
    {
        "Owner" => owner: String,
        "OfferSequence" => offer_sequence: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Condition" => condition: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Fulfillment" => fulfillment: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "EscrowFinish",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "OfferSequence": 7
        });
        let tx = EscrowFinish::from_json(&json).unwrap();
        assert_eq!(tx.offer_sequence, 7);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "EscrowFinish");
    }
}
