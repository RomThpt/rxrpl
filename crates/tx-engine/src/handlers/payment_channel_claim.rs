use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::owner_dir::remove_from_owner_dir;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct PaymentChannelClaimTransactor;

const TF_CLOSE: u32 = 0x0001_0000;

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

impl Transactor for PaymentChannelClaimTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        parse_channel(ctx.tx)?;
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

        // tx Account must be source or destination
        let account_str = helpers::get_account(ctx.tx)?;
        let ch_src = channel["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let ch_dst = channel["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;

        if account_str != ch_src && account_str != ch_dst {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let channel_key = parse_channel(ctx.tx)?;
        let account_str = helpers::get_account(ctx.tx)?;
        let flags = helpers::get_flags(ctx.tx);

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
        let ch_balance: u64 = channel["Balance"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TransactionResult::TefInternal)?;

        let ch_src_str = channel["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let ch_dst_str = channel["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        // If Balance field in tx, update channel balance (claim)
        let claim_amount = if let Some(new_balance) = helpers::get_u64_str_field(ctx.tx, "Balance")
        {
            if new_balance > ch_amount {
                return Err(TransactionResult::TecUnfundedPayment);
            }
            if new_balance < ch_balance {
                return Err(TransactionResult::TemMalformed);
            }
            let delta = new_balance - ch_balance;
            channel["Balance"] = serde_json::Value::String(new_balance.to_string());
            delta
        } else {
            0
        };

        // Credit destination with claimed amount
        if claim_amount > 0 {
            let dst_id = decode_account_id(&ch_dst_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let dst_key = keylet::account(&dst_id);
            let dst_bytes = ctx
                .view
                .read(&dst_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut dst_account: serde_json::Value =
                serde_json::from_slice(&dst_bytes).map_err(|_| TransactionResult::TefInternal)?;
            let dst_balance = helpers::get_balance(&dst_account);
            helpers::set_balance(&mut dst_account, dst_balance + claim_amount);
            let dst_data =
                serde_json::to_vec(&dst_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(dst_key, dst_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        let should_close = flags & TF_CLOSE != 0;

        // tfClose semantics:
        //  - If caller is the channel SOURCE, defer the close: set Expiration
        //    to (now + SettleDelay). The channel persists until destination
        //    cashes out OR the expiration is reached on a later claim.
        //  - If caller is the channel DESTINATION, the close is immediate
        //    (destination can always release the locked funds back).
        let caller_is_source = account_str == ch_src_str;
        if should_close && caller_is_source {
            let settle_delay = channel
                .get("SettleDelay")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let now = ctx.view.parent_close_time();
            let expiration = now.saturating_add(settle_delay);
            channel["Expiration"] = serde_json::Value::from(expiration);
            let ch_data =
                serde_json::to_vec(&channel).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(channel_key, ch_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            // Bump source's sequence (caller).
            let src_id = decode_account_id(&ch_src_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let src_key = keylet::account(&src_id);
            let src_bytes = ctx
                .view
                .read(&src_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut src_account: serde_json::Value =
                serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut src_account);
            let src_data =
                serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(src_key, src_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            return Ok(TransactionResult::TesSuccess);
        }

        if should_close {
            let final_balance: u64 = channel["Balance"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let remaining = ch_amount.saturating_sub(final_balance);

            // Credit source with remaining funds
            if remaining > 0 {
                let src_id = decode_account_id(&ch_src_str)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                let src_key = keylet::account(&src_id);
                let src_bytes = ctx
                    .view
                    .read(&src_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut src_account: serde_json::Value = serde_json::from_slice(&src_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
                let src_balance = helpers::get_balance(&src_account);
                helpers::set_balance(&mut src_account, src_balance + remaining);
                helpers::adjust_owner_count(&mut src_account, -1);

                let account_id = decode_account_id(account_str)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                if account_id == src_id {
                    helpers::increment_sequence(&mut src_account);
                }

                let src_data =
                    serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(src_key, src_data)
                    .map_err(|_| TransactionResult::TefInternal)?;

                // If sender is destination, increment their sequence
                if account_id != src_id {
                    let sender_key = keylet::account(&account_id);
                    let sender_bytes = ctx
                        .view
                        .read(&sender_key)
                        .ok_or(TransactionResult::TerNoAccount)?;
                    let mut sender: serde_json::Value = serde_json::from_slice(&sender_bytes)
                        .map_err(|_| TransactionResult::TefInternal)?;
                    helpers::increment_sequence(&mut sender);
                    let sender_data =
                        serde_json::to_vec(&sender).map_err(|_| TransactionResult::TefInternal)?;
                    ctx.view
                        .update(sender_key, sender_data)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
            } else {
                // No remaining, but still decrement owner count and increment sequence
                let src_id = decode_account_id(&ch_src_str)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                let src_key = keylet::account(&src_id);
                let src_bytes = ctx
                    .view
                    .read(&src_key)
                    .ok_or(TransactionResult::TerNoAccount)?;
                let mut src_account: serde_json::Value = serde_json::from_slice(&src_bytes)
                    .map_err(|_| TransactionResult::TefInternal)?;
                helpers::adjust_owner_count(&mut src_account, -1);

                let account_id = decode_account_id(account_str)
                    .map_err(|_| TransactionResult::TemInvalidAccountId)?;
                if account_id == src_id {
                    helpers::increment_sequence(&mut src_account);
                }

                let src_data =
                    serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
                ctx.view
                    .update(src_key, src_data)
                    .map_err(|_| TransactionResult::TefInternal)?;

                if account_id != src_id {
                    let sender_key = keylet::account(&account_id);
                    let sender_bytes = ctx
                        .view
                        .read(&sender_key)
                        .ok_or(TransactionResult::TerNoAccount)?;
                    let mut sender: serde_json::Value = serde_json::from_slice(&sender_bytes)
                        .map_err(|_| TransactionResult::TefInternal)?;
                    helpers::increment_sequence(&mut sender);
                    let sender_data =
                        serde_json::to_vec(&sender).map_err(|_| TransactionResult::TefInternal)?;
                    ctx.view
                        .update(sender_key, sender_data)
                        .map_err(|_| TransactionResult::TefInternal)?;
                }
            }

            // Unlink from source's owner directory then delete the channel.
            let src_id_for_dir = decode_account_id(&ch_src_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            remove_from_owner_dir(ctx.view, &src_id_for_dir, &channel_key)?;
            ctx.view
                .erase(&channel_key)
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Just update channel state and increment sender sequence
            let ch_data =
                serde_json::to_vec(&channel).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(channel_key, ch_data)
                .map_err(|_| TransactionResult::TefInternal)?;

            let account_id = decode_account_id(account_str)
                .map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let account_key = keylet::account(&account_id);
            let account_bytes = ctx
                .view
                .read(&account_key)
                .ok_or(TransactionResult::TerNoAccount)?;
            let mut account: serde_json::Value = serde_json::from_slice(&account_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;
            helpers::increment_sequence(&mut account);
            let account_data =
                serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(account_key, account_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

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

        for (addr, id, balance, owner_count) in [
            (SRC, &src_id, 100_000_000u64, 1u32),
            (DST, &dst_id, 50_000_000, 0),
        ] {
            let key = keylet::account(id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 2,
                "OwnerCount": owner_count,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

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
    fn apply_claim_partial() {
        let (ledger, channel_id) = setup_with_channel();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "PaymentChannelClaim",
            "Account": DST,
            "Channel": channel_id,
            "Balance": "3000000",
            "Fee": "12",
            "Sequence": 2,
            "Flags": 0,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentChannelClaimTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Destination gets 3M
        let dst_id = decode_account_id(DST).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "53000000");
    }

    #[test]
    fn apply_close_channel() {
        let (ledger, channel_id) = setup_with_channel();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "PaymentChannelClaim",
            "Account": SRC,
            "Channel": channel_id,
            "Fee": "12",
            "Sequence": 2,
            "Flags": TF_CLOSE,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = PaymentChannelClaimTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Source-initiated close defers deletion: channel persists with
        // Expiration = now + SettleDelay (matching rippled behavior).
        let src_id = decode_account_id(SRC).unwrap();
        let dst_id = decode_account_id(DST).unwrap();
        let channel_key = keylet::pay_channel(&src_id, &dst_id, 1);
        assert!(sandbox.exists(&channel_key));
        let ch_bytes = sandbox.read(&channel_key).unwrap();
        let ch: serde_json::Value = serde_json::from_slice(&ch_bytes).unwrap();
        assert!(ch.get("Expiration").is_some());

        // Source balance unchanged on deferred close (no funds returned yet).
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "100000000");
        // OwnerCount unchanged because channel is not deleted (defer close).
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);
    }
}
