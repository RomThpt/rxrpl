use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Oracle {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    pub owner: String,
    #[serde(rename = "OracleDocumentID")]
    pub oracle_document_id: u32,
    pub last_update_time: u32,
    pub price_data_series: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(rename = "URI")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl LedgerObject for Oracle {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::Oracle
    }
    fn common(&self) -> &CommonLedgerFields {
        &self.common
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "LedgerEntryType": "Oracle",
            "Owner": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
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
        let obj: Oracle = serde_json::from_value(json).unwrap();
        assert_eq!(obj.owner, "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk");
        assert_eq!(obj.oracle_document_id, 1);
        assert_eq!(obj.price_data_series.len(), 1);
        let rt = serde_json::to_value(&obj).unwrap();
        assert_eq!(rt["OracleDocumentID"], 1);
    }
}
