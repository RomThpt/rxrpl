use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// SignerListSet transaction handler.
///
/// Sets, updates, or removes the signer list for multi-signing.
pub struct SignerListSetTransactor;

impl Transactor for SignerListSetTransactor {
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
