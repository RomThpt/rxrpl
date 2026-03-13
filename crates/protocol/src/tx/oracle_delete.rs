use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An OracleDelete transaction deletes a price oracle.
    OracleDelete => TransactionType::OracleDelete,
    {
        "OracleDocumentID" => oracle_document_id: u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "OracleDelete",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "OracleDocumentID": 1
        });
        let tx = OracleDelete::from_json(&json).unwrap();
        assert_eq!(tx.oracle_document_id, 1);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "OracleDelete");
    }
}
