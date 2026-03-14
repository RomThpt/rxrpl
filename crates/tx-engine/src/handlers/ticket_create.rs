use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// TicketCreate transaction handler.
///
/// Creates one or more tickets for future transaction submission.
pub struct TicketCreateTransactor;

impl Transactor for TicketCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let count = ctx
            .tx
            .get("TicketCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if count == 0 || count > 250 {
            return Err(TransactionResult::TemMalformed);
        }
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
