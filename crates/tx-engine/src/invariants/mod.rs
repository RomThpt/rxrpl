pub mod xrp_not_created;

use crate::view::sandbox::SandboxChanges;

/// Trait for invariant checks run after every transaction.
///
/// Invariant violations indicate a bug in the transactor logic
/// and should never occur in production.
pub trait InvariantCheck: Send + Sync {
    /// Human-readable name for this invariant.
    fn name(&self) -> &str;

    /// Check the invariant against the sandbox changes.
    ///
    /// `drops_before` is the total XRP supply before the transaction.
    /// Returns `Ok(())` if the invariant holds, or `Err(message)` if violated.
    fn check(
        &self,
        changes: &SandboxChanges,
        drops_before: u64,
        drops_after: u64,
    ) -> Result<(), String>;
}

/// Run all invariant checks on a set of changes.
pub fn run_invariant_checks(
    checks: &[Box<dyn InvariantCheck>],
    changes: &SandboxChanges,
    drops_before: u64,
    drops_after: u64,
) -> Result<(), String> {
    for check in checks {
        check.check(changes, drops_before, drops_after).map_err(|e| {
            format!("invariant '{}' violated: {}", check.name(), e)
        })?;
    }
    Ok(())
}

/// Create the default set of invariant checks.
pub fn default_invariant_checks() -> Vec<Box<dyn InvariantCheck>> {
    vec![Box::new(xrp_not_created::XrpNotCreated)]
}
