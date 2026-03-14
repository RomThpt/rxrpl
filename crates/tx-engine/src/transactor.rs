use rxrpl_amendment::Rules;
use rxrpl_protocol::TransactionResult;
use serde_json::Value;

use crate::fees::FeeSettings;
use crate::view::apply_view::ApplyView;
use crate::view::read_view::ReadView;

/// Context for the preflight (stateless) validation phase.
pub struct PreflightContext<'a> {
    /// The transaction JSON.
    pub tx: &'a Value,
    /// Current protocol rules.
    pub rules: &'a Rules,
    /// Fee settings.
    pub fees: &'a FeeSettings,
}

impl PreflightContext<'_> {
    /// Get the default base fee for a reference transaction.
    pub fn default_base_fee(&self) -> u64 {
        self.fees.base_fee
    }
}

/// Context for the preclaim (read-only ledger validation) phase.
pub struct PreclaimContext<'a> {
    /// The transaction JSON.
    pub tx: &'a Value,
    /// Read-only view of the ledger.
    pub view: &'a dyn ReadView,
    /// Current protocol rules.
    pub rules: &'a Rules,
}

/// Context for the apply (state mutation) phase.
pub struct ApplyContext<'a> {
    /// The transaction JSON.
    pub tx: &'a Value,
    /// Mutable view for state changes.
    pub view: &'a mut dyn ApplyView,
    /// Current protocol rules.
    pub rules: &'a Rules,
    /// Fee settings.
    pub fees: &'a FeeSettings,
}

/// Trait for transaction type handlers.
///
/// Each transaction type implements this trait to define its validation
/// and execution logic. Transactors are separate from the protocol
/// transaction structs to keep `protocol` as pure data definitions.
pub trait Transactor: Send + Sync {
    /// Stateless validation of the transaction fields.
    ///
    /// Called before any ledger state is consulted. Should validate
    /// field formats, flag combinations, and basic invariants.
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult>;

    /// Read-only ledger validation.
    ///
    /// Called after preflight, with access to ledger state but no
    /// mutations allowed. Should check that the transaction can
    /// succeed (account exists, sufficient balance, etc.).
    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult>;

    /// Calculate the base fee for this transaction type.
    ///
    /// Most transactions use the default base fee. Override for
    /// transactions with non-standard fees.
    fn calculate_base_fee(&self, ctx: &PreflightContext<'_>) -> u64 {
        ctx.default_base_fee()
    }

    /// Apply state mutations.
    ///
    /// Called after fee consumption. The sandbox ensures mutations
    /// can be rolled back if the transaction fails with a non-tec result.
    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult>;
}
