use crate::tx::macros::define_transaction;
use crate::types::TransactionType;

define_transaction! {
    /// A CheckCancel transaction cancels an unredeemed Check.
    CheckCancel => TransactionType::CheckCancel,
    {
        "CheckID" => check_id: String
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::common::Transaction;

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::json!({
            "TransactionType": "CheckCancel",
            "Account": "rN7n3473SaZBCG4dFL83w7p1W9cgZB6xkk",
            "Fee": "12",
            "CheckID": "838766BA2B995C00744175F69A1B11E32C3DBC40E64801A4056FCBD657F57334"
        });
        let tx = CheckCancel::from_json(&json).unwrap();
        assert_eq!(
            tx.check_id,
            "838766BA2B995C00744175F69A1B11E32C3DBC40E64801A4056FCBD657F57334"
        );
        let rt = tx.to_json().unwrap();
        assert_eq!(rt["TransactionType"], "CheckCancel");
    }
}
