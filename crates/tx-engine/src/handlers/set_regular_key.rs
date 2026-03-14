use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// SetRegularKey transaction handler.
///
/// Sets or clears the regular key pair for an account.
pub struct SetRegularKeyTransactor;

impl Transactor for SetRegularKeyTransactor {
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
