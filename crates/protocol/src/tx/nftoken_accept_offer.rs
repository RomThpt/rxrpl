use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An NFTokenAcceptOffer transaction accepts an offer to buy or sell an NFToken.
    NFTokenAcceptOffer => TransactionType::NFTokenAcceptOffer,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "NFTokenSellOffer" => nftoken_sell_offer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "NFTokenBuyOffer" => nftoken_buy_offer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "NFTokenBrokerFee" => nftoken_broker_fee: Option<serde_json::Value>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "NFTokenAcceptOffer",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "NFTokenSellOffer": "000800006203F49C21D5D6E022CB16DE3538F248662FC73C00000000000000000000000000000001"
        });
        let tx = NFTokenAcceptOffer::from_json(&json).unwrap();
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "NFTokenAcceptOffer");
    }
}
