use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct EscrowCancelTransactor;

impl Transactor for EscrowCancelTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "OfferSequence").ok_or(TransactionResult::TemMalformed)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let owner_str =
            helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
        let offer_seq =
            helpers::get_u32_field(ctx.tx, "OfferSequence").ok_or(TransactionResult::TemMalformed)?;

        let owner_id = decode_account_id(owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let escrow_key = keylet::escrow(&owner_id, offer_seq);

        let escrow_bytes = ctx
            .view
            .read(&escrow_key)
            .ok_or(TransactionResult::TecNoTarget)?;
        let escrow: serde_json::Value =
            serde_json::from_slice(&escrow_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // CancelAfter must be set and <= parent_close_time
        let cancel_after = escrow
            .get("CancelAfter")
            .and_then(|v| v.as_u64())
            .ok_or(TransactionResult::TecNoPermission)?;

        if (ctx.view.parent_close_time() as u64) < cancel_after {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let owner_str =
            helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
        let offer_seq =
            helpers::get_u32_field(ctx.tx, "OfferSequence").ok_or(TransactionResult::TemMalformed)?;

        let owner_id = decode_account_id(owner_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let escrow_key = keylet::escrow(&owner_id, offer_seq);

        let escrow_bytes = ctx
            .view
            .read(&escrow_key)
            .ok_or(TransactionResult::TecNoTarget)?;
        let escrow: serde_json::Value =
            serde_json::from_slice(&escrow_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let amount: u64 = escrow["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TransactionResult::TefInternal)?;

        // Credit source (owner) with the escrowed amount
        let owner_key = keylet::account(&owner_id);
        let owner_bytes = ctx
            .view
            .read(&owner_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut owner_account: serde_json::Value =
            serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let owner_balance = helpers::get_balance(&owner_account);
        helpers::set_balance(&mut owner_account, owner_balance + amount);
        helpers::adjust_owner_count(&mut owner_account, -1);

        // Increment sequence on tx sender
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        if account_id == owner_id {
            helpers::increment_sequence(&mut owner_account);
        }

        let owner_data =
            serde_json::to_vec(&owner_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(owner_key, owner_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // If sender is different from owner, increment sender's sequence
        if account_id != owner_id {
            let sender_key = keylet::account(&account_id);
            let sender_bytes = ctx
                .view
                .read(&sender_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut sender_account: serde_json::Value =
                serde_json::from_slice(&sender_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut sender_account);
            let sender_data = serde_json::to_vec(&sender_account)
                .map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(sender_key, sender_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Delete escrow
        ctx.view
            .erase(&escrow_key)
            .map_err(|_| TransactionResult::TefInternal)?;

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

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_escrow(cancel_after: u32) -> Ledger {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SRC,
            "Balance": "90000000",
            "Sequence": 2,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(src_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let escrow_key = keylet::escrow(&src_id, 1);
        let escrow = serde_json::json!({
            "LedgerEntryType": "Escrow",
            "Account": SRC,
            "Destination": DST,
            "Amount": "10000000",
            "FinishAfter": 100,
            "CancelAfter": cancel_after,
        });
        ledger
            .put_state(escrow_key, serde_json::to_vec(&escrow).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn apply_cancels_escrow() {
        let mut ledger = setup_with_escrow(200);
        ledger.header.parent_close_time = 300;

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "EscrowCancel",
            "Account": SRC,
            "Owner": SRC,
            "OfferSequence": 1,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = EscrowCancelTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify source got amount back
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "100000000");
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 0);

        // Verify escrow deleted
        let escrow_key = keylet::escrow(&src_id, 1);
        assert!(!sandbox.exists(&escrow_key));
    }
}
