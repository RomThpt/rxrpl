use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// An NFTokenMint transaction mints a new NFToken.
    NFTokenMint => TransactionType::NFTokenMint,
    {
        "NFTokenTaxon" => nftoken_taxon: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Issuer" => issuer: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "TransferFee" => transfer_fee: Option<u16>,
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
            "TransactionType": "NFTokenMint",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "NFTokenTaxon": 0,
            "TransferFee": 5000
        });
        let tx = NFTokenMint::from_json(&json).unwrap();
        assert_eq!(tx.nftoken_taxon, 0);
        assert_eq!(tx.transfer_fee, Some(5000));
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "NFTokenMint");
    }
}
