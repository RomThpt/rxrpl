use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::TransactionResult;
use rxrpl_protocol::keylet;
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// TicketCreate transaction handler.
///
/// Creates one or more tickets for future transaction submission.
/// Each ticket reserves a sequence number that can be used later.
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

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let key = keylet::account(&account_id);

        if !ctx.view.exists(&key) {
            return Err(TransactionResult::TerNoAccount);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemMalformed)?;
        let acct_key = keylet::account(&account_id);

        let count = ctx.tx["TicketCount"].as_u64().unwrap_or(0) as u32;

        // Read account
        let bytes = ctx
            .view
            .read(&acct_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut acct: Value =
            serde_json::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)?;

        let start_seq = helpers::get_sequence(&acct);
        // The TicketCreate tx itself consumes `start_seq`; tickets are
        // reserved starting at `start_seq + 1`, matching rippled.
        let first_ticket_seq = start_seq + 1;

        // Create tickets
        for i in 0..count {
            let ticket_seq = first_ticket_seq + i;
            let ticket_key = keylet::ticket(&account_id, ticket_seq);

            let ticket_obj = serde_json::json!({
                "LedgerEntryType": "Ticket",
                "Account": account_str,
                "TicketSequence": ticket_seq,
                "Flags": 0,
            });

            let ticket_bytes =
                serde_json::to_vec(&ticket_obj).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .insert(ticket_key, ticket_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
            crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &ticket_key)
                .map_err(|_| TransactionResult::TemMalformed)?;
        }

        // Update account: advance sequence past the tx itself + all tickets,
        // increase owner count.
        let new_seq = first_ticket_seq + count;
        acct["Sequence"] = Value::from(new_seq);
        helpers::adjust_owner_count(&mut acct, count as i32);

        let new_bytes = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}
