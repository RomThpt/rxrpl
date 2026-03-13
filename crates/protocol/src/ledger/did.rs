use serde::{Deserialize, Serialize};

use crate::ledger::common::{CommonLedgerFields, LedgerObject};
use crate::types::LedgerEntryType;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Did {
    #[serde(flatten)]
    pub common: CommonLedgerFields,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(rename = "DIDDocument")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub did_document: Option<String>,
    #[serde(rename = "URI")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl LedgerObject for Did {
    fn ledger_entry_type() -> LedgerEntryType {
        LedgerEntryType::DID
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
            "LedgerEntryType": "DID",
            "DIDDocument": "646F63",
            "URI": "68747470733A2F2F"
        });
        let obj: Did = serde_json::from_value(json).unwrap();
        assert_eq!(obj.did_document, Some("646F63".to_string()));
        assert_eq!(obj.uri, Some("68747470733A2F2F".to_string()));
        let rt = serde_json::to_value(&obj).unwrap();
        assert_eq!(rt["DIDDocument"], "646F63");
        assert_eq!(rt["URI"], "68747470733A2F2F");
    }
}
