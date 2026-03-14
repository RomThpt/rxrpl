use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// Payment transaction handler.
///
/// Handles XRP and IOU payments between accounts.
pub struct PaymentTransactor;

impl Transactor for PaymentTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Destination must be present
        if ctx.tx.get("Destination").is_none() {
            return Err(TransactionResult::TemDstIsObligatory);
        }
        // Amount must be present
        if ctx.tx.get("Amount").is_none() {
            return Err(TransactionResult::TemBadAmount);
        }
        Ok(())
    }

    fn preclaim(&self, _ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        // Full implementation: check destination exists, check balances, etc.
        Ok(())
    }

    fn apply(
        &self,
        _ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        // Full implementation: transfer XRP/IOU, update balances, handle paths
        Ok(TransactionResult::TesSuccess)
    }
}
