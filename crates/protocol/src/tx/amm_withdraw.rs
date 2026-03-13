use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMWithdraw transaction withdraws funds from an AMM instance.
    AMMWithdraw => TransactionType::AMMWithdraw,
    {
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount2" => amount2: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LPTokenIn" => lp_token_in: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "EPrice" => e_price: Option<serde_json::Value>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "AMMWithdraw",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Flags": 0x0002_0000u32,
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" }
        });
        let tx = AMMWithdraw::from_json(&json).unwrap();
        assert_eq!(tx.amount, None);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMWithdraw");
    }
}
