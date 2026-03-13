use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CheckCash transaction cashes a Check object.
    CheckCash => TransactionType::CheckCash,
    {
        "CheckID" => check_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        "Amount" => amount: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        "DeliverMin" => deliver_min: Option<serde_json::Value>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "CheckCash",
            "Account": "rfkE1aSy9G8Upk4JssnwBxhEv5p4mn2KTy",
            "Fee": "12",
            "CheckID": "838766BA2B995C00744175F69A1B11E32C3DBC40E64801A4056FCBD657F57334",
            "Amount": "1000000"
        });
        let tx = CheckCash::from_json(&json).unwrap();
        assert_eq!(tx.check_id, "838766BA2B995C00744175F69A1B11E32C3DBC40E64801A4056FCBD657F57334");
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "CheckCash");
    }
}
