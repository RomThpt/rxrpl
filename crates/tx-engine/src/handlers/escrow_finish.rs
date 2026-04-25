use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::remove_from_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct EscrowFinishTransactor;

impl Transactor for EscrowFinishTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Owner must be present
        helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;

        // OfferSequence must be present
        helpers::get_u32_field(ctx.tx, "OfferSequence").ok_or(TransactionResult::TemMalformed)?;

        // If Condition is set, Fulfillment must also be set (and vice versa)
        let has_condition = helpers::get_str_field(ctx.tx, "Condition").is_some();
        let has_fulfillment = helpers::get_str_field(ctx.tx, "Fulfillment").is_some();
        if has_condition != has_fulfillment {
            return Err(TransactionResult::TemMalformed);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let owner_str =
            helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
        let offer_seq = helpers::get_u32_field(ctx.tx, "OfferSequence")
            .ok_or(TransactionResult::TemMalformed)?;

        let owner_id =
            decode_account_id(owner_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let escrow_key = keylet::escrow(&owner_id, offer_seq);

        let escrow_bytes = ctx
            .view
            .read(&escrow_key)
            .ok_or(TransactionResult::TecNoTarget)?;
        let escrow: serde_json::Value =
            serde_json::from_slice(&escrow_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // If FinishAfter is set, parent_close_time must be >= FinishAfter
        if let Some(finish_after) = escrow.get("FinishAfter").and_then(|v| v.as_u64()) {
            if (ctx.view.parent_close_time() as u64) < finish_after {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let owner_str =
            helpers::get_str_field(ctx.tx, "Owner").ok_or(TransactionResult::TemMalformed)?;
        let offer_seq = helpers::get_u32_field(ctx.tx, "OfferSequence")
            .ok_or(TransactionResult::TemMalformed)?;

        let owner_id =
            decode_account_id(owner_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
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
        let destination_str = escrow["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;

        // Credit destination
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_key = keylet::account(&dst_id);

        if let Some(dst_bytes) = ctx.view.read(&dst_key) {
            let mut dst_account: serde_json::Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let dst_balance = helpers::get_balance(&dst_account);
            helpers::set_balance(&mut dst_account, dst_balance + amount);
            let dst_data =
                serde_json::to_vec(&dst_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Create destination account
            let new_account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": destination_str,
                "Balance": amount.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            let dst_data =
                serde_json::to_vec(&new_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .insert(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Unlink escrow from owner directory then delete it.
        remove_from_owner_dir(ctx.view, &owner_id, &escrow_key)?;
        ctx.view
            .erase(&escrow_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Decrement owner count on source
        let owner_key = keylet::account(&owner_id);
        let owner_bytes = ctx
            .view
            .read(&owner_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut owner_account: serde_json::Value =
            serde_json::from_slice(&owner_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut owner_account, -1);

        // Increment sequence on the transaction sender (not necessarily the owner)
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
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
            let mut sender_account: serde_json::Value = serde_json::from_slice(&sender_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut sender_account);
            let sender_data =
                serde_json::to_vec(&sender_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(sender_key, sender_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_escrow(src: &str, dst: &str, amount: u64, seq: u32) -> Ledger {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(src).unwrap();
        let src_key = keylet::account(&src_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": src,
            "Balance": "100000000",
            "Sequence": 2,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(src_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let escrow_key = keylet::escrow(&src_id, seq);
        let escrow = serde_json::json!({
            "LedgerEntryType": "Escrow",
            "Account": src,
            "Destination": dst,
            "Amount": amount.to_string(),
            "FinishAfter": 100,
        });
        ledger
            .put_state(escrow_key, serde_json::to_vec(&escrow).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_owner() {
        let tx = serde_json::json!({
            "TransactionType": "EscrowFinish",
            "Account": SRC,
            "OfferSequence": 1,
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            EscrowFinishTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_finishes_escrow() {
        let mut ledger = setup_with_escrow(SRC, DST, 10_000_000, 1);
        // Add destination account
        let dst_id = decode_account_id(DST).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": DST,
            "Balance": "5000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(dst_key, serde_json::to_vec(&dst_account).unwrap())
            .unwrap();

        // Set parent_close_time >= FinishAfter
        ledger.header.parent_close_time = 200;

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "EscrowFinish",
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

        let result = EscrowFinishTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify destination got the amount
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "15000000");

        // Verify escrow deleted
        let src_id = decode_account_id(SRC).unwrap();
        let escrow_key = keylet::escrow(&src_id, 1);
        assert!(!sandbox.exists(&escrow_key));
    }
}
