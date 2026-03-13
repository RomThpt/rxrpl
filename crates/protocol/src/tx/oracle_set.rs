use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An OracleSet transaction creates or updates a price oracle.
    OracleSet => TransactionType::OracleSet,
    {
        "OracleDocumentID" => oracle_document_id: u32,
        "LastUpdateTime" => last_update_time: u32,
        "PriceDataSeries" => price_data_series: Vec<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "AssetClass" => asset_class: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Provider" => provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "URI" => uri: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "OracleSet",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "OracleDocumentID": 1,
            "LastUpdateTime": 743609014,
            "PriceDataSeries": [
                {
                    "PriceData": {
                        "BaseAsset": "XRP",
                        "QuoteAsset": "USD",
                        "AssetPrice": 740,
                        "Scale": 3
                    }
                }
            ],
            "Provider": "70726F7669646572"
        });
        let tx = OracleSet::from_json(&json).unwrap();
        assert_eq!(tx.oracle_document_id, 1);
        assert_eq!(tx.price_data_series.len(), 1);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "OracleSet");
    }
}
