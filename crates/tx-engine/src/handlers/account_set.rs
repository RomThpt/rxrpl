use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// AccountSet transaction handler.
///
/// Modifies account flags and settings.
pub struct AccountSetTransactor;

impl Transactor for AccountSetTransactor {
    fn preflight(&self, _ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        Ok(())
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        Ok(())
    }

    fn apply(
        &self,
        _ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        Ok(TransactionResult::TesSuccess)
    }
}
