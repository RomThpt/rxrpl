use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};
use crate::view::read_view::ReadView;

/// Payment transaction handler.
///
/// Handles XRP-only payments between accounts. Validates source and destination
/// accounts, checks sufficient balance, and transfers drops between accounts.
/// If the destination does not exist, a new AccountRoot is created.
pub struct PaymentTransactor;

impl PaymentTransactor {
    /// Read an AccountRoot from the view and parse it as JSON.
    fn read_account(
        view: &dyn ReadView,
        account_id: &rxrpl_primitives::AccountId,
    ) -> Option<serde_json::Value> {
        let key = keylet::account(account_id);
        let bytes = view.read(&key)?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl Transactor for PaymentTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Destination must be present
        helpers::get_destination(ctx.tx)?;

        // Amount must be present and be a valid XRP drops string
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        // Amount must be positive
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        // Destination must differ from Account (no self-payment)
        let account = helpers::get_account(ctx.tx)?;
        let destination = helpers::get_destination(ctx.tx)?;

        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        // Parse source account
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Check source account exists and read its balance
        let src_account =
            Self::read_account(ctx.view, &src_id).ok_or(TransactionResult::TerNoAccount)?;

        // Parse destination account
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Check destination exists
        let dst_key = keylet::account(&dst_id);
        let _dst_exists = ctx.view.exists(&dst_key);

        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);

        // Check source has sufficient balance for amount + fee
        let src_balance = helpers::get_balance(&src_account);
        let total_cost = amount
            .checked_add(fee)
            .ok_or(TransactionResult::TemBadAmount)?;

        if src_balance < total_cost {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        // Parse account IDs
        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let src_key = keylet::account(&src_id);
        let dst_key = keylet::account(&dst_id);

        // Read and update source account
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&src_account);
        let new_src_balance = src_balance
            .checked_sub(amount)
            .ok_or(TransactionResult::TecUnfundedPayment)?;

        helpers::set_balance(&mut src_account, new_src_balance);
        helpers::increment_sequence(&mut src_account);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Read or create destination account
        if let Some(dst_bytes) = ctx.view.read(&dst_key) {
            // Destination exists: add amount to balance
            let mut dst_account: serde_json::Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;

            let dst_balance = helpers::get_balance(&dst_account);
            let new_dst_balance = dst_balance
                .checked_add(amount)
                .ok_or(TransactionResult::TefInternal)?;

            helpers::set_balance(&mut dst_account, new_dst_balance);

            let dst_data =
                serde_json::to_vec(&dst_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Destination does not exist: create a new AccountRoot
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

        Ok(TransactionResult::TesSuccess)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::FeeSettings;
    use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
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
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
    }

    fn make_payment_tx(
        account: &str,
        destination: &str,
        amount: &str,
        fee: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "Payment",
            "Account": account,
            "Destination": destination,
            "Amount": amount,
            "Fee": fee,
        })
    }

    // -- preflight tests --

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": SRC_ADDRESS,
            "Amount": "1000000",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemDstIsObligatory));
    }

    #[test]
    fn preflight_missing_amount() {
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": SRC_ADDRESS,
            "Destination": DST_ADDRESS,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadAmount));
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "0", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadAmount));
    }

    #[test]
    fn preflight_self_payment() {
        let tx = make_payment_tx(SRC_ADDRESS, SRC_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        let result = PaymentTransactor.preflight(&ctx);
        assert_eq!(result, Err(TransactionResult::TemBadSend));
    }

    #[test]
    fn preflight_valid() {
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert!(PaymentTransactor.preflight(&ctx).is_ok());
    }

    // -- preclaim tests --

    #[test]
    fn preclaim_source_not_found() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        let result = PaymentTransactor.preclaim(&ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }

    #[test]
    fn preclaim_insufficient_balance() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        let result = PaymentTransactor.preclaim(&ctx);
        assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
    }

    #[test]
    fn preclaim_valid_with_existing_destination() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(PaymentTransactor.preclaim(&ctx).is_ok());
    }

    #[test]
    fn preclaim_valid_create_account() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert!(PaymentTransactor.preclaim(&ctx).is_ok());
    }

    // -- apply tests --

    #[test]
    fn apply_transfer_to_existing_account() {
        let mut ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        add_account(&mut ledger, DST_ADDRESS, 5_000_000);

        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify source balance decreased and sequence incremented
        let src_id = decode_account_id(SRC_ADDRESS).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "9000000");
        assert_eq!(src["Sequence"].as_u64().unwrap(), 2);

        // Verify destination balance increased
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "6000000");
    }

    #[test]
    fn apply_creates_new_destination_account() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 10_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify destination was created with correct fields
        let dst_id = decode_account_id(DST_ADDRESS).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "1000000");
        assert_eq!(dst["LedgerEntryType"].as_str().unwrap(), "AccountRoot");
        assert_eq!(dst["Sequence"].as_u64().unwrap(), 1);
        assert_eq!(dst["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn apply_insufficient_balance() {
        let ledger = setup_ledger_with_account(SRC_ADDRESS, 500);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TecUnfundedPayment));
    }

    #[test]
    fn apply_source_not_found() {
        let ledger = Ledger::genesis();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let tx = make_payment_tx(SRC_ADDRESS, DST_ADDRESS, "1000000", "10");
        let rules = Rules::new();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentTransactor.apply(&mut ctx);
        assert_eq!(result, Err(TransactionResult::TerNoAccount));
    }
}
