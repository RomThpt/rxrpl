use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An NFTokenCancelOffer transaction cancels existing token offers.
    NFTokenCancelOffer => TransactionType::NFTokenCancelOffer,
    {
        "NFTokenOffers" => nftoken_offers: Vec<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "NFTokenCancelOffer",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "NFTokenOffers": [
                "000800006203F49C21D5D6E022CB16DE3538F248662FC73C00000000000000000000000000000001"
            ]
        });
        let tx = NFTokenCancelOffer::from_json(&json).unwrap();
        assert_eq!(tx.nftoken_offers.len(), 1);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "NFTokenCancelOffer");
    }
}
