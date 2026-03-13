use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMCreate transaction creates a new Automated Market Maker instance.
    AMMCreate => TransactionType::AMMCreate,
    {
        "Amount" => amount: serde_json::Value,
        "Amount2" => amount2: serde_json::Value,
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
            "TransactionType": "AMMCreate",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Amount": "1000000",
            "Amount2": {
                "value": "100",
                "currency": "USD",
                "issuer": "rIssuer111111111111111111111"
            },
            "TradingFee": 500
        });
        let tx = AMMCreate::from_json(&json).unwrap();
        assert_eq!(tx.trading_fee, 500);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMCreate");
    }
}
