use crate::invariants::InvariantCheck;
use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

/// Invariant: XRP cannot be created. The total supply must not increase.
///
/// After every transaction, the total XRP supply must be less than or
/// equal to what it was before. XRP can only be destroyed (fees).
pub struct XrpNotCreated;

impl InvariantCheck for XrpNotCreated {
    fn name(&self) -> &str {
        "XrpNotCreated"
    }

    fn check(
        &self,
        _changes: &SandboxChanges,
        drops_before: u64,
        drops_after: u64,
        _tx: Option<&Value>,
    ) -> Result<(), String> {
        if drops_after > drops_before {
            return Err(format!(
                "XRP supply increased from {drops_before} to {drops_after}"
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn empty_changes(destroyed: u64) -> SandboxChanges {
        SandboxChanges {
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: destroyed,
        }
    }

    #[test]
    fn xrp_supply_unchanged() {
        let check = XrpNotCreated;
        let changes = empty_changes(0);
        assert!(check.check(&changes, 100, 100, None).is_ok());
    }

    #[test]
    fn xrp_supply_decreased() {
        let check = XrpNotCreated;
        let changes = empty_changes(10);
        assert!(check.check(&changes, 100, 90, None).is_ok());
    }

    #[test]
    fn xrp_supply_increased_fails() {
        let check = XrpNotCreated;
        let changes = empty_changes(0);
        assert!(check.check(&changes, 100, 110, None).is_err());
    }
}
