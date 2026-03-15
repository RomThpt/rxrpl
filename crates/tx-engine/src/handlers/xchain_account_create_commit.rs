use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

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
        let amount =
            helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
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
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;

        // Bridge must exist
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let door_id = decode_account_id(locking_door)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Check sufficient balance for Amount + SignatureReward + Fee
        let amount =
            helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward =
            helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&src_account);
        if balance < amount + sig_reward + fee {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let destination_str = helpers::get_destination(ctx.tx)?.to_string();
        let amount =
            helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward =
            helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;

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
        helpers::increment_sequence(&mut src_account);
        helpers::adjust_owner_count(&mut src_account, 1);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment bridge's XChainAccountCreateCount
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let door_id = decode_account_id(locking_door)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&door_id, &bridge_data);

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
        let new_count = current_count + 1;
        bridge_entry["XChainAccountCreateCount"] =
            serde_json::Value::String(new_count.to_string());

        let bridge_data_updated =
            serde_json::to_vec(&bridge_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(bridge_key, bridge_data_updated)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create XChainOwnedCreateAccountClaimID entry
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, new_count);
        let create_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedCreateAccountClaimID",
            "Account": account_str,
            "XChainBridge": bridge,
            "XChainAccountCreateCount": new_count.to_string(),
            "Destination": destination_str,
            "Amount": amount.to_string(),
            "SignatureReward": sig_reward.to_string(),
            "Attestations": [],
            "Flags": 0,
        });

        let create_data =
            serde_json::to_vec(&create_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(create_key, create_data)
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
        for (addr, balance) in
            [(DOOR, 100_000_000u64), (USER, 100_000_000), (SENDER, 50_000_000)]
        {
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
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
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

        // Verify sender balance decreased by Amount + SignatureReward
        let sender_id = decode_account_id(SENDER).unwrap();
        let src_key = keylet::account(&sender_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "39999900"); // 50M - 10M - 100
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);

        // Verify bridge counter incremented
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let bridge_bytes = sandbox.read(&bridge_key).unwrap();
        let bridge_entry: serde_json::Value =
            serde_json::from_slice(&bridge_bytes).unwrap();
        assert_eq!(
            bridge_entry["XChainAccountCreateCount"].as_str().unwrap(),
            "1"
        );

        // Verify create account claim ID entry exists
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, 1);
        let create_bytes = sandbox.read(&create_key).unwrap();
        let create: serde_json::Value =
            serde_json::from_slice(&create_bytes).unwrap();
        assert_eq!(
            create["LedgerEntryType"].as_str().unwrap(),
            "XChainOwnedCreateAccountClaimID"
        );
        assert_eq!(create["Destination"].as_str().unwrap(), NEW_ACCOUNT);
        assert_eq!(create["Amount"].as_str().unwrap(), "10000000");
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
