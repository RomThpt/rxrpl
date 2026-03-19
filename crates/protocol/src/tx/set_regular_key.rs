use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A SetRegularKey transaction assigns or changes the regular key pair for an account.
    SetRegularKey => TransactionType::SetRegularKey,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "RegularKey" => regular_key: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "SetRegularKey",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "RegularKey": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
        });
        let tx = SetRegularKey::from_json(&json).unwrap();
        assert_eq!(
            tx.regular_key.as_deref(),
            Some("rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy")
        );
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "SetRegularKey");
    }
}
