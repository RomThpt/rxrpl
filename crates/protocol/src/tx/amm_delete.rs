use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMDelete transaction deletes an empty AMM instance.
    AMMDelete => TransactionType::AMMDelete,
    {
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AMMDelete",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" }
        });
        let tx = AMMDelete::from_json(&json).unwrap();
        assert!(tx.asset.is_object());
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMDelete");
    }
}
