use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CheckCashTransactor;

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

impl Transactor for CheckCashTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // CheckID must be present
        parse_check_id(ctx.tx)?;

        // Exactly one of Amount or DeliverMin must be present
        let has_amount = helpers::get_xrp_amount(ctx.tx).is_some();
        let has_deliver_min = helpers::get_u64_str_field(ctx.tx, "DeliverMin").is_some();
        if has_amount == has_deliver_min {
            return Err(TransactionResult::TemMalformed);
        }

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

        // tx Account must be the check's Destination
        let account_str = helpers::get_account(ctx.tx)?;
        let check_dst = check["Destination"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        if account_str != check_dst {
            return Err(TransactionResult::TecNoPermission);
        }

        // Check must not be expired
        if let Some(expiration) = check.get("Expiration").and_then(|v| v.as_u64()) {
            if (ctx.view.parent_close_time() as u64) >= expiration {
                return Err(TransactionResult::TecExpired);
            }
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let check_key = parse_check_id(ctx.tx)?;

        let check_bytes = ctx
            .view
            .read(&check_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let check: serde_json::Value =
            serde_json::from_slice(&check_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let send_max: u64 = check["SendMax"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(TransactionResult::TefInternal)?;

        // Determine cash amount
        let cash_amount = if let Some(amount) = helpers::get_xrp_amount(ctx.tx) {
            if amount > send_max {
                return Err(TransactionResult::TecInsufficientPayment);
            }
            amount
        } else {
            let deliver_min =
                helpers::get_u64_str_field(ctx.tx, "DeliverMin").ok_or(TransactionResult::TemMalformed)?;
            if deliver_min > send_max {
                return Err(TransactionResult::TecInsufficientPayment);
            }
            send_max
        };

        // Debit check source
        let check_src_str = check["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let check_src_id = decode_account_id(check_src_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let check_src_key = keylet::account(&check_src_id);

        let check_src_bytes = ctx
            .view
            .read(&check_src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut check_src_account: serde_json::Value =
            serde_json::from_slice(&check_src_bytes)
                .map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&check_src_account);
        if src_balance < cash_amount {
            return Err(TransactionResult::TecUnfundedPayment);
        }
        helpers::set_balance(&mut check_src_account, src_balance - cash_amount);
        helpers::adjust_owner_count(&mut check_src_account, -1);

        let check_src_data = serde_json::to_vec(&check_src_account)
            .map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(check_src_key, check_src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Credit destination (tx sender)
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let account_key = keylet::account(&account_id);

        let account_bytes = ctx
            .view
            .read(&account_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut account: serde_json::Value =
            serde_json::from_slice(&account_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let dst_balance = helpers::get_balance(&account);
        helpers::set_balance(&mut account, dst_balance + cash_amount);
        helpers::increment_sequence(&mut account);

        let account_data =
            serde_json::to_vec(&account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(account_key, account_data)
            .map_err(|_| TransactionResult::TefInternal)?;

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
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const SRC: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const DST: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

    fn setup_with_check(src: &str, dst: &str, send_max: u64) -> (Ledger, String) {
        let mut ledger = Ledger::genesis();
        let src_id = decode_account_id(src).unwrap();
        let dst_id = decode_account_id(dst).unwrap();

        for (addr, id, balance) in [
            (src, &src_id, 100_000_000u64),
            (dst, &dst_id, 50_000_000),
        ] {
            let key = keylet::account(id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": if addr == src { 1 } else { 0 },
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
            "SendMax": send_max.to_string(),
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
    fn preflight_both_amount_and_deliver_min() {
        let tx = serde_json::json!({
            "TransactionType": "CheckCash",
            "Account": DST,
            "CheckID": "0".repeat(64),
            "Amount": "1000000",
            "DeliverMin": "500000",
            "Fee": "12",
        });
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext { tx: &tx, rules: &rules, fees: &fees };
        assert_eq!(
            CheckCashTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_cashes_check_with_amount() {
        let (ledger, check_id) = setup_with_check(SRC, DST, 5_000_000);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "CheckCash",
            "Account": DST,
            "CheckID": check_id,
            "Amount": "3000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CheckCashTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Source debited
        let src_id = decode_account_id(SRC).unwrap();
        let src_key = keylet::account(&src_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "97000000");

        // Destination credited
        let dst_id = decode_account_id(DST).unwrap();
        let dst_key = keylet::account(&dst_id);
        let dst_bytes = sandbox.read(&dst_key).unwrap();
        let dst: serde_json::Value = serde_json::from_slice(&dst_bytes).unwrap();
        assert_eq!(dst["Balance"].as_str().unwrap(), "53000000");
    }
}
