use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A PaymentChannelFund transaction adds XRP to an open payment channel.
    PaymentChannelFund => TransactionType::PaymentChannelFund,
    {
        "Channel" => channel: String,
        "Amount" => amount: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Expiration" => expiration: Option<u32>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "PaymentChannelFund",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Channel": "C1AE6DDDEEC05CF2978C0BAD6FE302948E9533691DC749DCDD3B9E5992CA6198",
            "Amount": "1000000"
        });
        let tx = PaymentChannelFund::from_json(&json).unwrap();
        assert_eq!(tx.channel, "C1AE6DDDEEC05CF2978C0BAD6FE302948E9533691DC749DCDD3B9E5992CA6198");
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "PaymentChannelFund");
    }
}
