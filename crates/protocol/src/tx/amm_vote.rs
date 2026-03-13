use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMVote transaction votes on the trading fee for an AMM instance.
    AMMVote => TransactionType::AMMVote,
    {
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value,
        "TradingFee" => trading_fee: u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AMMVote",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" },
            "TradingFee": 600
        });
        let tx = AMMVote::from_json(&json).unwrap();
        assert_eq!(tx.trading_fee, 600);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMVote");
    }
}
