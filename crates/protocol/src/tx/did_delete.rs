use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A DIDDelete transaction deletes a DID.
    DIDDelete => TransactionType::DIDDelete,
    {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "DIDDelete",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12"
        });
        let tx = DIDDelete::from_json(&json).unwrap();
        assert_eq!(DIDDelete::transaction_type(), TransactionType::DIDDelete);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "DIDDelete");
    }
}
