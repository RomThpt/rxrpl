use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::keylet;
use rxrpl_protocol::TransactionResult;
use serde_json::Value;

use crate::helpers;
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

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        if !ctx.view.exists(&key) {
            return Err(TransactionResult::TerNoAccount);
        }

        // Check the offer exists
        let offer_seq = ctx.tx["OfferSequence"].as_u64().unwrap_or(0) as u32;
        let offer_key = keylet::offer(&account_id, offer_seq);
        if !ctx.view.exists(&offer_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let acct_key = keylet::account(&account_id);

        let offer_seq = ctx.tx["OfferSequence"].as_u64().unwrap_or(0) as u32;
        let offer_key = keylet::offer(&account_id, offer_seq);

        // Delete the offer
        ctx.view
            .erase(&offer_key)
            .map_err(|_| TransactionResult::TecNoEntry)?;

        // Update account: increment sequence, decrement owner count
        let bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        helpers::increment_sequence(&mut acct);
        helpers::adjust_owner_count(&mut acct, -1);

        let new_bytes =
            serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}
