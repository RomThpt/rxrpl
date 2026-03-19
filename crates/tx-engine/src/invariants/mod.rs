pub mod account_root_not_deleted;
pub mod ledger_entry_types_match;
pub mod nftoken_count_tracking;
pub mod nftoken_page_bounds;
pub mod no_bad_offers;
pub mod no_deep_freeze_without_freeze;
pub mod no_negative_balance;
pub mod no_xrp_trust_lines;
pub mod no_zero_balance_entries;
pub mod no_zero_escrow;
pub mod transaction_fee_check;
pub mod valid_amm;
pub mod valid_clawback;
pub mod valid_ledger_entry_type;
pub mod valid_mpt_issuance;
pub mod valid_new_account_root;
pub mod valid_permissioned_domain;
pub mod xrp_balance_range;
pub mod xrp_not_created;

use crate::view::sandbox::SandboxChanges;
use serde_json::Value;

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
    /// `tx` is the transaction JSON, or `None` for pseudo-transactions.
    /// Returns `Ok(())` if the invariant holds, or `Err(message)` if violated.
    fn check(
        &self,
        changes: &SandboxChanges,
        drops_before: u64,
        drops_after: u64,
        tx: Option<&Value>,
    ) -> Result<(), String>;
}

/// Run all invariant checks on a set of changes.
pub fn run_invariant_checks(
    checks: &[Box<dyn InvariantCheck>],
    changes: &SandboxChanges,
    drops_before: u64,
    drops_after: u64,
    tx: Option<&Value>,
) -> Result<(), String> {
    for check in checks {
        check
            .check(changes, drops_before, drops_after, tx)
            .map_err(|e| format!("invariant '{}' violated: {}", check.name(), e))?;
    }
    Ok(())
}

/// Create the default set of invariant checks.
pub fn default_invariant_checks() -> Vec<Box<dyn InvariantCheck>> {
    vec![
        Box::new(xrp_not_created::XrpNotCreated),
        Box::new(xrp_balance_range::XrpBalanceRange),
        Box::new(no_negative_balance::NoNegativeBalance),
        Box::new(account_root_not_deleted::AccountRootNotDeleted),
        Box::new(valid_ledger_entry_type::ValidLedgerEntryType),
        Box::new(ledger_entry_types_match::LedgerEntryTypesMatch),
        Box::new(no_xrp_trust_lines::NoXrpTrustLines),
        Box::new(no_bad_offers::NoBadOffers),
        Box::new(no_zero_balance_entries::NoZeroBalanceEntries),
        Box::new(valid_new_account_root::ValidNewAccountRoot),
        Box::new(transaction_fee_check::TransactionFeeCheck),
        Box::new(nftoken_page_bounds::NFTokenPageBounds),
        Box::new(nftoken_count_tracking::NFTokenCountTracking),
        Box::new(no_deep_freeze_without_freeze::NoDeepFreezeWithoutFreeze),
        Box::new(no_zero_escrow::NoZeroEscrow),
        Box::new(valid_clawback::ValidClawback),
        Box::new(valid_amm::ValidAmm),
        Box::new(valid_mpt_issuance::ValidMptIssuance),
        Box::new(valid_permissioned_domain::ValidPermissionedDomain),
    ]
}
