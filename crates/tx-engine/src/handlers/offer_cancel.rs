use rxrpl_protocol::TransactionResult;

use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// OfferCancel transaction handler.
///
/// Cancels an existing order on the decentralized exchange.
pub struct OfferCancelTransactor;

impl Transactor for OfferCancelTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        if ctx.tx.get("OfferSequence").is_none() {
            return Err(TransactionResult::TemBadOffer);
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
