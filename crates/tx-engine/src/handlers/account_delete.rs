use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};
use serde_json::Value;

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

/// AccountDelete transaction handler.
///
/// Deletes an account and transfers remaining XRP to a destination.
/// Requires owner count == 0 and charges an elevated fee (5x reserve increment).
pub struct AccountDeleteTransactor;

/// Ledger flag: destination requires deposit authorization.
const LSF_DEPOSIT_AUTH: u32 = 0x01000000;

/// Ledger flag: destination requires a destination tag.
const LSF_REQUIRE_DEST_TAG: u32 = 0x00020000;

impl Transactor for AccountDeleteTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Destination must be present
        let destination = helpers::get_destination(ctx.tx)?;

        // Account must not equal Destination
        let account = helpers::get_account(ctx.tx)?;
        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        Ok(())
    }

    fn calculate_base_fee(&self, ctx: &PreflightContext<'_>) -> u64 {
        // AccountDelete costs 5x the owner reserve increment (default 10 XRP)
        ctx.fees.reserve_increment * 5
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        // Source must exist
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let src_key = keylet::account(&src_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let src_obj: Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Source must have no owned objects. A TicketSequence the tx is using
        // is consumed by the AccountDelete itself, so its Ticket SLE doesn't
        // count toward outstanding obligations.
        let mut owner_count = helpers::get_owner_count(&src_obj);
        if let Some(ticket_seq) = helpers::get_u32_field(ctx.tx, "TicketSequence") {
            let ticket_key = keylet::ticket(&src_id, ticket_seq);
            if ctx.view.exists(&ticket_key) {
                owner_count = owner_count.saturating_sub(1);
            }
        }
        if owner_count > 0 {
            return Err(TransactionResult::TecHasObligations);
        }

        // Destination must exist
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = ctx.view.read(&dst_key).ok_or(TransactionResult::TecNoDst)?;
        let dst_obj: Value =
            serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // If destination has deposit auth, check for preauthorization
        let dst_flags = helpers::get_flags(&dst_obj);
        if dst_flags & LSF_DEPOSIT_AUTH != 0 {
            let preauth_key = keylet::deposit_preauth(&dst_id, &src_id);
            if !ctx.view.exists(&preauth_key) {
                return Err(TransactionResult::TecNoPermission);
            }
        }

        // If destination requires dest tag, check DestinationTag is present
        if dst_flags & LSF_REQUIRE_DEST_TAG != 0
            && helpers::get_u32_field(ctx.tx, "DestinationTag").is_none()
        {
            return Err(TransactionResult::TecDstTagNeeded);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let src_key = keylet::account(&src_id);
        let dst_key = keylet::account(&dst_id);

        // If the tx uses a TicketSequence, consume the matching Ticket SLE.
        // Mirrors rippled's generic ticket consumption path.
        if let Some(ticket_seq) = helpers::get_u32_field(ctx.tx, "TicketSequence") {
            let ticket_key = keylet::ticket(&src_id, ticket_seq);
            if ctx.view.exists(&ticket_key) {
                crate::owner_dir::remove_from_owner_dir(ctx.view, &src_id, &ticket_key)
                    .map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .erase(&ticket_key)
                    .map_err(|_| TransactionResult::TefInternal)?;
            }
        }

        // Read source account
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_obj: Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Get remaining balance (fee already deducted by engine)
        let remaining = helpers::get_balance(&src_obj);

        // Transfer remaining balance to destination
        if remaining > 0 {
            let dst_bytes = ctx.view.read(&dst_key).ok_or(TransactionResult::TecNoDst)?;
            let mut dst_obj: Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

            let dst_balance = helpers::get_balance(&dst_obj);
            let new_dst_balance = dst_balance
                .checked_add(remaining)
                .ok_or(TransactionResult::TefInternal)?;
            helpers::set_balance(&mut dst_obj, new_dst_balance);

            let dst_data =
                serde_json::to_vec(&dst_obj).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Zero out source balance and delete
        helpers::set_balance(&mut src_obj, 0);
        // Set OwnerCount to 0 explicitly for invariant check
        src_obj["OwnerCount"] = Value::from(0u64);

        // We need to update before erase so the deleted data has balance==0
        let src_data = serde_json::to_vec(&src_obj).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Erase the account
        ctx.view
            .erase(&src_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC_ADDRESS: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST_ADDRESS: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_ledger_with_account(address: &str, balance: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let data = serde_json::to_vec(&account).unwrap();
        ledger.put_state(key, data).unwrap();
        ledger
    }

    fn add_account(ledger: &mut Ledger, address: &str, balance: u64) {
        add_account_with_flags(ledger, address, balance, 0);
    }

    fn add_account_with_flags(ledger: &mut Ledger, address: &str, balance: u64, flags: u32) {
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": address,
            "Balance": balance.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": flags,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn make_account_delete_tx(account: &str, destination: &str) -> Value {
        serde_json::json!({
            "TransactionType": "AccountDelete",
            "Account": account,
            "Destination": destination,
            "Fee": "10000000",
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "AccountDelete",
            "Account": SRC_ADDRESS,
            "Fee": "10000000",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AccountDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemDstIsObligatory)
        );
    }

    #[test]
    fn preflight_self_delete() {
        let tx = make_account_delete_tx(SRC_ADDRESS, SRC_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            AccountDeleteTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadSend)
        );
    }

    #[test]
    fn preflight_valid() {
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(AccountDeleteTransactor.preflight(&ctx).is_ok());
    }

    #[test]
    fn calculate_base_fee_is_5x_increment() {
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        // 2_000_000 * 5 = 10_000_000 (10 XRP)
        assert_eq!(AccountDeleteTransactor.calculate_base_fee(&ctx), 10_000_000);
    }

    // -- preclaim tests --

    #[test]
    fn preclaim_source_not_found() {
        let mut ledger = Ledger::genesis();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TerNoAccount)
        );
    }

    #[test]
    fn preclaim_has_obligations() {
        let mut ledger = Ledger::genesis();
        // Source with owner_count > 0
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        let src = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SRC_ADDRESS,
            "Balance": "10000000",
            "Sequence": 1,
            "OwnerCount": 2,
            "Flags": 0,
        });
        ledger
            .put_state(src_key, serde_json::to_vec(&src).unwrap())
            .unwrap();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecHasObligations)
        );
    }

    #[test]
    fn preclaim_destination_not_found() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoDst)
        );
    }

    #[test]
    fn preclaim_deposit_auth_no_preauth() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_DEPOSIT_AUTH);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoPermission)
        );
    }

    #[test]
    fn preclaim_require_dest_tag_missing() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_REQUIRE_DEST_TAG);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            AccountDeleteTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDstTagNeeded)
        );
    }

    #[test]
    fn preclaim_require_dest_tag_present() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account_with_flags(&mut ledger, DST_ADDRESS, 5_000_000, LSF_REQUIRE_DEST_TAG);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        tx["DestinationTag"] = Value::from(42);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(AccountDeleteTransactor.preclaim(&ctx).is_ok());
    }

    #[test]
    fn preclaim_valid() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(AccountDeleteTransactor.preclaim(&ctx).is_ok());
    }

    // -- apply tests --

    #[test]
    fn apply_deletes_account_and_transfers_balance() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AccountDeleteTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Source should be erased
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        assert!(sandbox.read(&src_key).is_none());

        // Destination should have received the balance
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "15000000");
    }

    #[test]
    fn apply_source_not_found() {
        let mut ledger = Ledger::genesis();
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_account_delete_tx(SRC_ADDRESS, DST_ADDRESS);
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = AccountDeleteTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }
}
