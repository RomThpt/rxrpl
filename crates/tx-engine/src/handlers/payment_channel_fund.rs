use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PaymentChannelFundTransactor;

fn parse_channel(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "Channel").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Hash256::new(arr))
}

impl Transactor for PaymentChannelFundTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        parse_channel(ctx.tx)?;

        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let channel_key = parse_channel(ctx.tx)?;

        let ch_bytes = ctx
            .view
            .read(&channel_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let channel: serde_json::Value =
            serde_json::from_slice(&ch_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // tx Account must be channel source
        let account_str = helpers::get_account(ctx.tx)?;
        let ch_src = channel["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if account_str != ch_src {
            return Err(TransactionResult::TecNoPermission);
        }

        // Check sufficient balance
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
        let channel_key = parse_channel(ctx.tx)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let account_str = helpers::get_account(ctx.tx)?;

        // Update channel Amount
        let ch_bytes = ctx
            .view
            .read(&channel_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut channel: serde_json::Value =
            serde_json::from_slice(&ch_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let ch_amount: u64 = channel["Amount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TransactionResult::TefInternal)?;
        channel["Amount"] = serde_json::Value::String((ch_amount + amount).to_string());

        // Update Expiration if provided
        if let Some(expiration) = helpers::get_u32_field(ctx.tx, "Expiration") {
            channel["Expiration"] = serde_json::Value::from(expiration);
        }

        let ch_data = serde_json::to_vec(&channel).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(channel_key, ch_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Debit sender
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let account_key = keylet::account(&account_id);
        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let balance = helpers::get_balance(&account);
        helpers::set_balance(
            &mut account,
            balance
                .checked_sub(amount)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
        helpers::increment_sequence(&mut account);

        let account_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(account_key, account_data)
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

    fn setup_with_channel() -> (Ledger, String) {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(SRC).unwrap();
        let dst_id = decode_account_id(DST).unwrap();

        let src_key = keylet::account(&src_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SRC,
            "Balance": "100000000",
            "Sequence": 2,
            "OwnerCount": 1,
            "Flags": 0,
        });
        ledger
            .put_state(src_key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let channel_key = keylet::pay_channel(&src_id, &dst_id, 1);
        let channel = serde_json::json!({
            "LedgerEntryType": "PayChannel",
            "Account": SRC,
            "Destination": DST,
            "Amount": "10000000",
            "Balance": "0",
            "SettleDelay": 86400,
            "Flags": 0,
        });
        ledger
            .put_state(channel_key, serde_json::to_vec(&channel).unwrap())
            .unwrap();

        let channel_id = hex::encode(channel_key.as_bytes());
        (ledger, channel_id)
    }

    #[test]
    fn apply_funds_channel() {
        let (ledger, channel_id) = setup_with_channel();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "PaymentChannelFund",
            "Account": SRC,
            "Channel": channel_id,
            "Amount": "5000000",
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentChannelFundTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify channel amount increased
        let src_id = decode_account_id(SRC).unwrap();
        let dst_id = decode_account_id(DST).unwrap();
        let channel_key = keylet::pay_channel(&src_id, &dst_id, 1);
        let ch_bytes = sandbox.read(&channel_key).unwrap();
        let ch: serde_json::Value = serde_json::from_slice(&ch_bytes).unwrap();
        assert_eq!(ch["Amount"].as_str().unwrap(), "15000000");

        // Verify sender debited
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "95000000");
    }
}
