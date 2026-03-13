use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A TrustSet transaction creates or modifies a trust line.
    TrustSet => TransactionType::TrustSet,
    {
        "LimitAmount" => limit_amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "QualityIn" => quality_in: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "QualityOut" => quality_out: Option<u32>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "TrustSet",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "LimitAmount": {
                "value": "100",
                "currency": "USD",
                "issuer": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
            }
        });
        let tx = TrustSet::from_json(&json).unwrap();
        assert!(tx.limit_amount.is_object());
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "TrustSet");
    }
}
