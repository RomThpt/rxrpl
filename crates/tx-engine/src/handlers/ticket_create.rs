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

        // Create tickets. rippled's Ticket SLE (ltTICKET, ledger_entries.macro)
        // has sfFlags (common SoeRequired field) and sfOwnerNode as SoeRequired,
        // so both are always serialized — Flags=0 and the owner-directory page
        // number (0 for the root page) — plus the threaded PreviousTxnID
        // (placeholder here; the engine stamps it).
        for i in 0..count {
            let ticket_seq = first_ticket_seq + i;
            let ticket_key = keylet::ticket(&account_id, ticket_seq);

            let owner_node = crate::owner_dir::add_to_owner_dir(ctx.view, &account_id, &ticket_key)
                .map_err(|_| TransactionResult::TemMalformed)?;

            let ticket_obj = serde_json::json!({
                "LedgerEntryType": "Ticket",
                "Account": account_str,
                "Flags": 0,
                "OwnerNode": format!("{owner_node:016X}"),
                "TicketSequence": ticket_seq,
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::ApplyContext;
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const ACCT: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";

    #[test]
    fn ticket_sle_carries_flags_and_owner_node() {
        let mut ledger = Ledger::genesis();
        let id = decode_account_id(ACCT).unwrap();
        let acct = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": ACCT,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&acct).unwrap())
            .unwrap();

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "TicketCreate",
            "Account": ACCT,
            "TicketCount": 1,
            "Fee": "12",
            "Sequence": 1,
        });
        // Engine consumes the sender's Sequence centrally before doApply.
        crate::handlers::central_consume_for_test(&mut sandbox, &tx);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            TicketCreateTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        // The ticket keyed at the post-consume account Sequence (2) carries the
        // common SoeRequired Flags=0 and the SoeRequired OwnerNode (0 = root
        // directory page), both always serialized by rippled.
        let ticket_key = keylet::ticket(&id, 2);
        let ticket: Value = serde_json::from_slice(&sandbox.read(&ticket_key).unwrap()).unwrap();
        assert_eq!(ticket["Flags"].as_u64().unwrap(), 0);
        assert_eq!(ticket["OwnerNode"].as_str().unwrap(), "0000000000000000");
        assert_eq!(ticket["TicketSequence"].as_u64().unwrap(), 2);
    }
}
