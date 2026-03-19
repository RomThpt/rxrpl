use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CheckCancelTransactor;

fn parse_check_id(tx: &serde_json::Value) -> Result<Hash256, TransactionResult> {
    let hex_str = helpers::get_str_field(tx, "CheckID").ok_or(TransactionResult::TemMalformed)?;
    let bytes = hex::decode(hex_str).map_err(|_| TransactionResult::TemMalformed)?;
    if bytes.len() != 32 {
        return Err(TransactionResult::TemMalformed);
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(Hash256::new(arr))
}

impl Transactor for CheckCancelTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        parse_check_id(ctx.tx)?;
        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let check_key = parse_check_id(ctx.tx)?;

        let check_bytes = ctx
            .view
            .read(&check_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let check: serde_json::Value =
            serde_json::from_slice(&check_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // tx Account must be the source or destination of the check
        let account_str = helpers::get_account(ctx.tx)?;
        let check_src = check["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let check_dst = check["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;

        if account_str != check_src && account_str != check_dst {
            return Err(TransactionResult::TecNoPermission);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let check_key = parse_check_id(ctx.tx)?;

        let check_bytes = ctx
            .view
            .read(&check_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let check: serde_json::Value =
            serde_json::from_slice(&check_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Decrement owner count on check source
        let check_src_str = check["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let check_src_id =
            decode_account_id(check_src_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let check_src_key = keylet::account(&check_src_id);

        let check_src_bytes = ctx
            .view
            .read(&check_src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut check_src_account: serde_json::Value =
            serde_json::from_slice(&check_src_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut check_src_account, -1);

        // Increment sequence on tx sender
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        if account_id == check_src_id {
            helpers::increment_sequence(&mut check_src_account);
        }

        let check_src_data =
            serde_json::to_vec(&check_src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(check_src_key, check_src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // If sender is different from check source, increment sender's sequence
        if account_id != check_src_id {
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

        // Delete check
        ctx.view
            .erase(&check_key)
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

    fn setup_with_check(src: &str, dst: &str) -> (Ledger, String) {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(src).unwrap();
        let dst_id = decode_account_id(dst).unwrap();

        for (addr, id, balance, owner_count) in [
            (src, &src_id, 100_000_000u64, 1u32),
            (dst, &dst_id, 50_000_000, 0),
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

        let check_key = keylet::check(&src_id, 1);
        let check = serde_json::json!({
            "LedgerEntryType": "Check",
            "Account": src,
            "Destination": dst,
            "SendMax": "5000000",
            "Sequence": 1,
            "Flags": 0,
        });
        ledger
            .put_state(check_key, serde_json::to_vec(&check).unwrap())
            .unwrap();

        let check_id_hex = hex::encode(check_key.as_bytes());
        (ledger, check_id_hex)
    }

    #[test]
    fn apply_cancel_by_source() {
        let (ledger, check_id) = setup_with_check(SRC, DST);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "CheckCancel",
            "Account": SRC,
            "CheckID": check_id,
            "Fee": "12",
            "Sequence": 2,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CheckCancelTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Check source owner count decremented
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 0);
    }
}
