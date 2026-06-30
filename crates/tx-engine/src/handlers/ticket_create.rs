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

        // Owner reserve (rippled CreateTicket::doApply): each of the `count`
        // tickets adds 1 to OwnerCount, so the account must fund the reserve for
        // the new owner count. rippled compares its `mPriorBalance` (the XRP
        // balance *before* the fee) against `accountReserve(OwnerCount + count)`
        // and returns tecINSUFFICIENT_RESERVE — fee and sequence charged, no
        // tickets created — when it falls short. The engine consumed the fee
        // centrally before doApply, so reconstruct mPriorBalance by adding it
        // back; the post-consume OwnerCount already reflects a spent Ticket.
        let owner_count = helpers::get_owner_count(&acct);
        let prior_balance = helpers::get_balance(&acct).saturating_add(helpers::get_fee(ctx.tx));
        if prior_balance < ctx.fees.account_reserve(owner_count + count) {
            return Err(TransactionResult::TecInsufficientReserve);
        }

        // The engine already consumed the tx's own Sequence/Ticket centrally
        // (parent sandbox). rippled's `firstTicketSeq` is the AccountRoot
        // Sequence *after* that consume: for a sequence-based tx it is the
        // bumped value (tx Sequence + 1), and for a ticketed tx it is the
        // unchanged account Sequence. Reading it here from the post-consume
        // account mirrors both cases.
        let first_ticket_seq = helpers::get_sequence(&acct);

        // Create tickets. rippled's Ticket SLE carries OwnerNode and the
        // threaded PreviousTxnID (placeholder here; the engine stamps it), and
        // omits Flags when zero.
        for i in 0..count {
            let ticket_seq = first_ticket_seq + i;
            let ticket_key = keylet::ticket(&account_id, ticket_seq);

            let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &ticket_key)
                .map_err(|_| TransactionResult::TemMalformed)?;

            let mut ticket_obj = serde_json::json!({
                "LedgerEntryType": "Ticket",
                "Account": account_str,
                "TicketSequence": ticket_seq,
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            // OwnerNode is omitted when zero (rippled drops default U64 fields).
            if owner_node != 0 {
                ticket_obj["OwnerNode"] = Value::from(format!("{owner_node:016X}"));
            }

            let ticket_bytes =
                serde_json::to_vec(&ticket_obj).map_err(|_| TransactionResult::TemMalformed)?;
            ctx.view
                .insert(ticket_key, ticket_bytes)
                .map_err(|_| TransactionResult::TemMalformed)?;
        }

        // Update account: advance sequence past the tx itself + all tickets,
        // increase owner count, and track the live ticket count.
        let new_seq = first_ticket_seq + count;
        acct["Sequence"] = Value::from(new_seq);
        helpers::adjust_owner_count(&mut acct, count as i32);
        let ticket_count = acct
            .get("TicketCount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        acct["TicketCount"] = Value::from(ticket_count + count);

        let new_bytes = serde_json::to_vec(&acct).map_err(|_| TransactionResult::TemMalformed)?;
        ctx.view
            .update(acct_key, new_bytes)
            .map_err(|_| TransactionResult::TemMalformed)?;

        Ok(TransactionResult::TesSuccess)
    }
}
