use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::add_to_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PaymentChannelCreateTransactor;

impl Transactor for PaymentChannelCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        helpers::get_destination(ctx.tx)?;

        let settle_delay =
            helpers::get_u32_field(ctx.tx, "SettleDelay").ok_or(TransactionResult::TemMalformed)?;
        if settle_delay == 0 {
            return Err(TransactionResult::TemMalformed);
        }

        // PublicKey required
        helpers::get_str_field(ctx.tx, "PublicKey").ok_or(TransactionResult::TemMalformed)?;

        let account = helpers::get_account(ctx.tx)?;
        let destination = helpers::get_destination(ctx.tx)?;
        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, src_account) = helpers::read_account_by_address(ctx.view, account_str)?;

        let destination_str = helpers::get_destination(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, destination_str)?;

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
        let dst_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Update source account
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
        let tx_seq = helpers::get_sequence(&src_account);
        helpers::increment_sequence(&mut src_account);
        helpers::adjust_owner_count(&mut src_account, 1);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create PayChannel entry
        let channel_key = keylet::pay_channel(&src_id, &dst_id, tx_seq);

        let mut channel = serde_json::json!({
            "LedgerEntryType": "PayChannel",
            "Account": account_str,
            "Destination": destination_str,
            "Amount": amount.to_string(),
            "Balance": "0",
            "SettleDelay": helpers::get_u32_field(ctx.tx, "SettleDelay").unwrap(),
            "PublicKey": helpers::get_str_field(ctx.tx, "PublicKey").unwrap(),
            "Flags": 0,
        });

        if let Some(cancel_after) = helpers::get_u32_field(ctx.tx, "CancelAfter") {
            channel["CancelAfter"] = serde_json::Value::from(cancel_after);
        }
        if let Some(dst_tag) = helpers::get_u32_field(ctx.tx, "DestinationTag") {
            channel["DestinationTag"] = serde_json::Value::from(dst_tag);
        }

        let channel_data =
            serde_json::to_vec(&channel).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(channel_key, channel_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        add_to_owner_dir(ctx.view, &src_id, &channel_key)?;

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

    fn setup_two_accounts() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [(SRC, 100_000_000u64), (DST, 50_000_000)] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 0,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }
        ledger
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "PaymentChannelCreate",
            "Account": SRC,
            "Destination": DST,
            "Amount": "0",
            "SettleDelay": 86400,
            "PublicKey": "0330E7FC9D56BB25D6893BA3F317AE5BCF33B3291BD63DB32654A313222F7FD020",
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
            PaymentChannelCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn apply_creates_channel() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "PaymentChannelCreate",
            "Account": SRC,
            "Destination": DST,
            "Amount": "10000000",
            "SettleDelay": 86400,
            "PublicKey": "0330E7FC9D56BB25D6893BA3F317AE5BCF33B3291BD63DB32654A313222F7FD020",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentChannelCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify source balance decreased
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "90000000");
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);

        // Verify channel exists
        let dst_id = decode_account_id(DST).unwrap();
        let channel_key = keylet::pay_channel(&src_id, &dst_id, 1);
        let ch_bytes = sandbox.read(&channel_key).unwrap();
        let ch: serde_json::Value = serde_json::from_slice(&ch_bytes).unwrap();
        assert_eq!(ch["Amount"].as_str().unwrap(), "10000000");
        assert_eq!(ch["Balance"].as_str().unwrap(), "0");
    }
}
