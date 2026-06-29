use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct LoanDeleteTransactor;

fn loan_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "LoanID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    Hash256::from_slice(&bytes).map_err(|_| TransactionResult::TemMalformed)
}

impl Transactor for LoanDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        loan_id(ctx.tx)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;
        let loan_key = loan_id(ctx.tx)?;
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;
        // A loan with outstanding principal cannot be deleted.
        if loan.get("PrincipalOutstanding").is_some() {
            return Err(TransactionResult::TecHasObligations);
        }
        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let loan_key = loan_id(ctx.tx)?;
        let loan_bytes = ctx
            .view
            .read(&loan_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let loan: serde_json::Value =
            serde_json::from_slice(&loan_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let borrower_id = decode_account_id(
            loan["Borrower"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let broker_key = {
            let id = loan["LoanBrokerID"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?;
            Hash256::from_slice(&hex::decode(id).map_err(|_| TransactionResult::TefInternal)?)
                .map_err(|_| TransactionResult::TefInternal)?
        };
        let broker_bytes = ctx
            .view
            .read(&broker_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut broker: serde_json::Value =
            serde_json::from_slice(&broker_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let broker_pseudo_id = decode_account_id(
            broker["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?,
        )
        .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Unlink the loan from the broker-pseudo and borrower directories, then
        // erase it.
        crate::owner_dir::remove_from_owner_dir(ctx.view, &broker_pseudo_id, &loan_key)?;
        crate::owner_dir::remove_from_owner_dir(ctx.view, &borrower_id, &loan_key)?;
        ctx.view
            .erase(&loan_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Broker: drop the active-loan count (removed when it reaches zero).
        let broker_loans = broker["OwnerCount"].as_u64().unwrap_or(0);
        if broker_loans <= 1 {
            broker.as_object_mut().unwrap().remove("OwnerCount");
        } else {
            broker["OwnerCount"] = serde_json::Value::from(broker_loans - 1);
        }
        ctx.view
            .update(
                broker_key,
                serde_json::to_vec(&broker).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        // Borrower: drop the owned-Loan count.
        let borrower_key = keylet::account(&borrower_id);
        let bb = ctx
            .view
            .read(&borrower_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut borrower_acct: serde_json::Value =
            serde_json::from_slice(&bb).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut borrower_acct, -1);
        // The submitter's Sequence/Ticket (and fee) are consumed centrally by the
        // engine before doApply — whether the submitter is the borrower or not.
        ctx.view
            .update(
                borrower_key,
                serde_json::to_vec(&borrower_acct).map_err(|_| TransactionResult::TefInternal)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}
