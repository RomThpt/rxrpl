use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainAccountCreateCommitTransactor;

impl Transactor for XChainAccountCreateCommitTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        let bridge = ctx
            .tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;
        bridge_helpers::serialize_bridge_spec(bridge)?;

        // Destination is required
        helpers::get_destination(ctx.tx)?;

        // Amount is required and must be > 0
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        // SignatureReward is required
        helpers::get_u64_str_field(ctx.tx, "SignatureReward")
            .ok_or(TransactionResult::TemMalformed)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, src_account) = helpers::read_account_by_address(ctx.view, account_str)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap();

        // Bridge must exist (on either chain's door).
        let bridge_key = bridge_helpers::find_bridge_keylet(bridge, |k| ctx.view.exists(k))?;
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Check sufficient balance for Amount + SignatureReward + Fee
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&src_account);
        if balance < amount + sig_reward + fee {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();

        // Deduct Amount + SignatureReward from sender
        let src_key = keylet::account(&account_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&src_account);
        let total_deduct = amount + sig_reward;
        helpers::set_balance(
            &mut src_account,
            src_balance
                .checked_sub(total_deduct)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment the bridge's XChainAccountCreateCount and lock the funds
        // (Amount + SignatureReward) in this chain's door account.
        let bridge_key = bridge_helpers::find_bridge_keylet(&bridge, |k| ctx.view.exists(k))?;
        let bridge_bytes = ctx
            .view
            .read(&bridge_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut bridge_entry: serde_json::Value =
            serde_json::from_slice(&bridge_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let current_count: u64 = bridge_entry["XChainAccountCreateCount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        bridge_entry["XChainAccountCreateCount"] =
            serde_json::Value::String((current_count + 1).to_string());

        let bridge_data_updated =
            serde_json::to_vec(&bridge_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(bridge_key, bridge_data_updated)
            .map_err(|_| TransactionResult::TefInternal)?;

        let door_str = bridge_entry["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?;
        let door_id =
            decode_account_id(door_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let door_key = keylet::account(&door_id);
        let door_bytes = ctx
            .view
            .read(&door_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut door_account: serde_json::Value =
            serde_json::from_slice(&door_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let door_balance = helpers::get_balance(&door_account);
        helpers::set_balance(&mut door_account, door_balance + total_deduct);
        let door_data =
            serde_json::to_vec(&door_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(door_key, door_data)
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

    const DOOR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const USER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const SENDER: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
    const NEW_ACCOUNT: &str = "rGWrZyQqhTp9Xu7G5iFQmGEFRWKvRm3TGr";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    fn setup_with_bridge() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [
            (DOOR, 100_000_000u64),
            (USER, 100_000_000),
            (SENDER, 50_000_000),
        ] {
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

        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_key = bridge_helpers::bridge_keylet_for_door(
            &door_id,
            bridge_spec().get("LockingChainIssue").unwrap(),
        );
        let bridge_entry = serde_json::json!({
            "LedgerEntryType": "Bridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "MinAccountCreateAmount": "10000000",
            "XChainClaimID": "0",
            "XChainAccountCreateCount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(bridge_key, serde_json::to_vec(&bridge_entry).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAccountCreateCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "Amount": "10000000",
            "SignatureReward": "100",
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
            XChainAccountCreateCommitTransactor.preflight(&ctx),
            Err(TransactionResult::TemDstIsObligatory)
        );
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAccountCreateCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "Destination": NEW_ACCOUNT,
            "Amount": "0",
            "SignatureReward": "100",
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
            XChainAccountCreateCommitTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn preclaim_insufficient_balance() {
        let ledger = setup_with_bridge();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAccountCreateCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "Destination": NEW_ACCOUNT,
            "Amount": "999999999999",
            "SignatureReward": "100",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainAccountCreateCommitTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfundedPayment)
        );
    }

    #[test]
    fn apply_creates_account_claim() {
        let ledger = setup_with_bridge();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAccountCreateCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainAccountCreateCommitTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify sender balance decreased by Amount + SignatureReward (no owner change)
        let sender_id = decode_account_id(SENDER).unwrap();
        let src_key = keylet::account(&sender_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "39999900"); // 50M - 10M - 100
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 0);

        // Verify bridge counter incremented
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_key = bridge_helpers::bridge_keylet_for_door(
            &door_id,
            bridge_spec().get("LockingChainIssue").unwrap(),
        );
        let bridge_bytes = sandbox.read(&bridge_key).unwrap();
        let bridge_entry: serde_json::Value = serde_json::from_slice(&bridge_bytes).unwrap();
        assert_eq!(
            bridge_entry["XChainAccountCreateCount"].as_str().unwrap(),
            "1"
        );

        // Verify the door account was credited with Amount + SignatureReward
        let door_key = keylet::account(&door_id);
        let door_bytes = sandbox.read(&door_key).unwrap();
        let door: serde_json::Value = serde_json::from_slice(&door_bytes).unwrap();
        assert_eq!(door["Balance"].as_str().unwrap(), "110000100"); // 100M + 10M + 100
    }

    #[test]
    fn preclaim_no_bridge() {
        let mut ledger = Ledger::genesis();
        let sender_id = decode_account_id(SENDER).unwrap();
        let key = keylet::account(&sender_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": SENDER,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger
            .put_state(key, serde_json::to_vec(&account).unwrap())
            .unwrap();

        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "XChainAccountCreateCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainAccountCreateCommitTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }
}
