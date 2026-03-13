use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMBid transaction bids on the auction slot of an AMM instance.
    AMMBid => TransactionType::AMMBid,
    {
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "BidMin" => bid_min: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "BidMax" => bid_max: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "AuthAccounts" => auth_accounts: Option<Vec<serde_json::Value>>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AMMBid",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" },
            "BidMin": {
                "value": "10",
                "currency": "03930D02208264E2E40EC1B0C09E4DB96EE197B1",
                "issuer": "rAMMAccount1111111111111111111"
            }
        });
        let tx = AMMBid::from_json(&json).unwrap();
        assert!(tx.bid_min.is_some());
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMBid");
    }
}
