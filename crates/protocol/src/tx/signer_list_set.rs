use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A SignerListSet transaction creates, replaces, or removes a signer list.
    SignerListSet => TransactionType::SignerListSet,
    {
        "SignerQuorum" => signer_quorum: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        "SignerEntries" => signer_entries: Option<Vec<serde_json::Value>>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "SignerListSet",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "SignerQuorum": 3,
            "SignerEntries": [
                {
                    "SignerEntry": {
                        "Account": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
                        "SignerWeight": 2
                    }
                }
            ]
        });
        let tx = SignerListSet::from_json(&json).unwrap();
        assert_eq!(tx.signer_quorum, 3);
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["SignerQuorum"], 3);
    }
}
