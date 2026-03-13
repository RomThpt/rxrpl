use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An AMMDeposit transaction deposits funds into an AMM instance.
    AMMDeposit => TransactionType::AMMDeposit,
    {
        "Asset" => asset: serde_json::Value,
        "Asset2" => asset2: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount2" => amount2: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "LPTokenOut" => lp_token_out: Option<serde_json::Value>,
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
            "TransactionType": "AMMDeposit",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Flags": 0x0008_0000u32,
            "Asset": { "currency": "XRP" },
            "Asset2": { "currency": "USD", "issuer": "rIssuer111111111111111111111" },
            "Amount": "1000000"
        });
        let tx = AMMDeposit::from_json(&json).unwrap();
        assert_eq!(tx.amount, Some(serde_json::json!("1000000")));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "AMMDeposit");
    }
}
