use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct CheckCreateTransactor;

impl Transactor for CheckCreateTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        helpers::get_destination(ctx.tx)?;

        // SendMax must be present and positive
        let send_max =
            helpers::get_u64_str_field(ctx.tx, "SendMax").ok_or(TransactionResult::TemBadAmount)?;
        if send_max == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        // Destination != Account
        let account = helpers::get_account(ctx.tx)?;
        let destination = helpers::get_destination(ctx.tx)?;
        if account == destination {
            return Err(TransactionResult::TemBadSend);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let destination_str = helpers::get_destination(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, destination_str)?;

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let destination_str = helpers::get_destination(ctx.tx)?;

        let src_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        // Update source account
        let src_key = keylet::account(&src_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let tx_seq = helpers::get_sequence(&src_account);
        helpers::increment_sequence(&mut src_account);
        helpers::adjust_owner_count(&mut src_account, 1);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create Check entry
        let check_key = keylet::check(&src_id, tx_seq);

        let mut check = serde_json::json!({
            "LedgerEntryType": "Check",
            "Account": account_str,
            "Destination": destination_str,
            "SendMax": helpers::get_u64_str_field(ctx.tx, "SendMax")
                .unwrap_or(0)
                .to_string(),
            "Sequence": tx_seq,
            "Flags": 0,
        });

        if let Some(expiration) = helpers::get_u32_field(ctx.tx, "Expiration") {
            check["Expiration"] = serde_json::Value::from(expiration);
        }

        let check_data = serde_json::to_vec(&check).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(check_key, check_data)
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
    fn preflight_self_check() {
        let tx = serde_json::json!({
            "TransactionType": "CheckCreate",
            "Account": SRC,
            "Destination": SRC,
            "SendMax": "1000000",
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
            CheckCreateTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadSend)
        );
    }

    #[test]
    fn apply_creates_check() {
        let ledger = setup_two_accounts();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "CheckCreate",
            "Account": SRC,
            "Destination": DST,
            "SendMax": "5000000",
            "Fee": "12",
            "Sequence": 1,
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = CheckCreateTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify check entry exists
        let src_id = decode_account_id(SRC).unwrap();
        let check_key = keylet::check(&src_id, 1);
        let check_bytes = sandbox.read(&check_key).unwrap();
        let check: serde_json::Value = serde_json::from_slice(&check_bytes).unwrap();
        assert_eq!(check["SendMax"].as_str().unwrap(), "5000000");
        assert_eq!(check["Destination"].as_str().unwrap(), DST);
    }
}
