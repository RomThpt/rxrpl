use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A DepositPreauth transaction pre-authorizes an account to deliver payments.
    DepositPreauth => TransactionType::DepositPreauth,
    {
        #[serde(skip_serializing_if = "Option::is_none")]
        "Authorize" => authorize: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Unauthorize" => unauthorize: Option<String>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "DepositPreauth",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "Authorize": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy"
        });
        let tx = DepositPreauth::from_json(&json).unwrap();
        assert_eq!(
            tx.authorize.as_deref(),
            Some("rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy")
        );
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "DepositPreauth");
    }
}
