use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A DIDSet transaction creates or updates a DID.
    DIDSet => TransactionType::DIDSet,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "Data" => data: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DIDDocument" => did_document: Option<String>,
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
            "TransactionType": "DIDSet",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "DIDDocument": "646F63",
            "URI": "68747470733A2F2F"
        });
        let tx = DIDSet::from_json(&json).unwrap();
        assert_eq!(tx.did_document, Some("646F63".to_string()));
        assert_eq!(tx.uri, Some("68747470733A2F2F".to_string()));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "DIDSet");
    }
}
