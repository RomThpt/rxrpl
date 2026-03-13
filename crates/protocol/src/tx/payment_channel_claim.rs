use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A PaymentChannelClaim transaction claims XRP from a payment channel.
    PaymentChannelClaim => TransactionType::PaymentChannelClaim,
    {
        "Channel" => channel: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Balance" => balance: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Signature" => signature: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "PublicKey" => public_key: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "PaymentChannelClaim",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Channel": "C1AE6DDDEEC05CF2978C0BAD6FE302948E9533691DC749DCDD3B9E5992CA6198",
            "Balance": "1000000",
            "Amount": "1000000"
        });
        let tx = PaymentChannelClaim::from_json(&json).unwrap();
        assert_eq!(tx.channel, "C1AE6DDDEEC05CF2978C0BAD6FE302948E9533691DC749DCDD3B9E5992CA6198");
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "PaymentChannelClaim");
    }
}
