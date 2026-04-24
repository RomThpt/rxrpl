use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct EscrowCreateTransactor;

impl Transactor for EscrowCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // Amount must be positive XRP
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        // Destination must be present
        helpers::get_destination(ctx.tx)?;

        // Must have at least FinishAfter or Condition
        let has_finish_after = helpers::get_u32_field(ctx.tx, "FinishAfter").is_some();
        let has_condition = helpers::get_str_field(ctx.tx, "Condition").is_some();
        if !has_finish_after && !has_condition {
            return Err(TransactionResult::TemMalformed);
        }

        // If both CancelAfter and FinishAfter, CancelAfter must be > FinishAfter
        if let (Some(cancel), Some(finish)) = (
            helpers::get_u32_field(ctx.tx, "CancelAfter"),
            helpers::get_u32_field(ctx.tx, "FinishAfter"),
        ) {
            if cancel <= finish {
                return Err(TransactionResult::TemBadExpiration);
            }
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, src_account) = helpers::read_account_by_address(ctx.view, account_str)?;

        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&src_account);

        if balance < amount + fee {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Read and update source account
        let src_key = keylet::account(&src_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&src_account);
        helpers::set_balance(
            &mut src_account,
            src_balance
                .checked_sub(amount)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
        helpers::increment_sequence(&mut src_account);
        helpers::adjust_owner_count(&mut src_account, 1);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create Escrow ledger entry
        let tx_seq = helpers::get_sequence(&src_account) - 1; // sequence was already incremented
        let escrow_key = keylet::escrow(&src_id, tx_seq);

        let mut escrow = serde_json::json!({
            "LedgerEntryType": "Escrow",
            "Account": account_str,
            "Destination": destination_str,
            "Amount": amount.to_string(),
        });

        if let Some(finish_after) = helpers::get_u32_field(ctx.tx, "FinishAfter") {
            escrow["FinishAfter"] = serde_json::Value::from(finish_after);
        }
        if let Some(cancel_after) = helpers::get_u32_field(ctx.tx, "CancelAfter") {
            escrow["CancelAfter"] = serde_json::Value::from(cancel_after);
        }
        if let Some(condition) = helpers::get_str_field(ctx.tx, "Condition") {
            escrow["Condition"] = serde_json::Value::String(condition.to_string());
        }

        let escrow_data =
            serde_json::to_vec(&escrow).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(escrow_key, escrow_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        add_to_owner_dir(ctx.view, &src_id, &escrow_key)?;

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

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_ledger(address: &str, balance: u64) -> Ledger {
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
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_finish_and_condition() {
        let tx = serde_json::json!({
            "TransactionType": "EscrowCreate",
            "Account": SRC,
            "Destination": DST,
            "Amount": "1000000",
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
            EscrowCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_cancel_before_finish() {
        let tx = serde_json::json!({
            "TransactionType": "EscrowCreate",
            "Account": SRC,
            "Destination": DST,
            "Amount": "1000000",
            "Fee": "12",
            "FinishAfter": 100,
            "CancelAfter": 50,
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            EscrowCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadExpiration)
        );
    }

    #[test]
    fn apply_creates_escrow() {
        let ledger = setup_ledger(SRC, 100_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "EscrowCreate",
            "Account": SRC,
            "Destination": DST,
            "Amount": "10000000",
            "Fee": "12",
            "Sequence": 1,
            "FinishAfter": 1000,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = EscrowCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify source balance decreased
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "90000000");
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);

        // Verify escrow entry exists
        let escrow_key = keylet::escrow(&src_id, 1);
        let escrow_bytes = sandbox.read(&escrow_key).unwrap();
        let escrow: serde_json::Value = serde_json::from_slice(&escrow_bytes).unwrap();
        assert_eq!(escrow["Amount"].as_str().unwrap(), "10000000");
        assert_eq!(escrow["Destination"].as_str().unwrap(), DST);
    }
}
