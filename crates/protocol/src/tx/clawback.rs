use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A Clawback transaction recovers funds from a token holder.
    Clawback => TransactionType::Clawback,
    {
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Holder" => holder: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "Clawback",
            "Account": "rIssuer111111111111111111111",
            "Fee": "12",
            "Amount": {
                "value": "100",
                "currency": "USD",
                "issuer": "rHolder222222222222222222222"
            }
        });
        let tx = Clawback::from_json(&json).unwrap();
        assert!(tx.amount.is_object());
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "Clawback");
    }
}
