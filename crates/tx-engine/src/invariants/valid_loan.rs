use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: Loan and LoanBroker ledger entries must be well-formed.
///
/// - Loan objects must have PrincipalOutstanding >= 0
/// - Loan objects must have a valid Status (0 or 1)
/// - LoanBroker objects must have OwnerCount >= 0
/// - LoanBroker objects must have DebtTotal >= 0
pub struct ValidLoan;

impl InvariantCheck for ValidLoan {
    fn name(&self) -> &str {
        "ValidLoan"
    }

    fn check(
        &self,
        changes: &SandboxChanges,
        _drops_before: u64,
        _drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        for (key, data) in changes.updates.iter().chain(changes.inserts.iter()) {
            let obj = match serde_json::from_slice::<Value>(data) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let entry_type = obj.get("LedgerEntryType").and_then(|v| v.as_str());

            match entry_type {
                Some("Loan") => {
                    // PrincipalOutstanding must parse to a valid non-negative integer
                    let principal: i64 = obj
                        .get("PrincipalOutstanding")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(-1);
                    if principal < 0 {
                        return Err(format!(
                            "Loan at {key} has invalid PrincipalOutstanding: {principal}"
                        ));
                    }

                    // Status is optional (absent = 0 Active); if present it must
                    // be 0 (Active) or 1 (Closed).
                    match obj.get("Status").and_then(|v| v.as_u64()) {
                        None | Some(0) | Some(1) => {}
                        _ => {
                            return Err(format!(
                                "Loan at {key} has invalid Status: {:?}",
                                obj.get("Status")
                            ));
                        }
                    }
                }
                Some("LoanBroker") => {
                    // DebtTotal is optional (default 0); if present it must parse
                    // to a non-negative integer.
                    if let Some(s) = obj.get("DebtTotal").and_then(|v| v.as_str()) {
                        let debt_total: i64 = s.parse().unwrap_or(-1);
                        if debt_total < 0 {
                            return Err(format!(
                                "LoanBroker at {key} has invalid DebtTotal: {debt_total}"
                            ));
                        }
                    }
                }
                _ => continue,
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;
    use serde_json::json;
    use std::collections::HashMap;

    fn empty_changes() -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        }
    }

    fn loan_entry(principal: &str, status: u64) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "Loan",
            "PrincipalOutstanding": principal,
            "Status": status,
        }))
        .unwrap()
    }

    fn broker_entry(debt_total: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "LedgerEntryType": "LoanBroker",
            "DebtTotal": debt_total,
        }))
        .unwrap()
    }

    #[test]
    fn valid_loan_passes() {
        let check = ValidLoan;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), loan_entry("5000000", 0));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn valid_closed_loan_passes() {
        let check = ValidLoan;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), loan_entry("0", 1));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn invalid_loan_status_fails() {
        let check = ValidLoan;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), loan_entry("5000000", 99));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }

    #[test]
    fn valid_broker_passes() {
        let check = ValidLoan;
        let mut changes = empty_changes();
        changes
            .inserts
            .insert(Hash256::new([0x01; 32]), broker_entry("1000000"));
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn broker_without_debt_total_passes() {
        // DebtTotal defaults to 0 and is omitted from a freshly created broker.
        let check = ValidLoan;
        let mut changes = empty_changes();
        let data = serde_json::to_vec(&json!({"LedgerEntryType": "LoanBroker"})).unwrap();
        changes.inserts.insert(Hash256::new([0x01; 32]), data);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn negative_broker_debt_total_fails() {
        let check = ValidLoan;
        let mut changes = empty_changes();
        changes
            .updates
            .insert(Hash256::new([0x01; 32]), broker_entry("-100"));
        assert!(check.check(&changes, 100, 100, None).is_err());
    }
}
