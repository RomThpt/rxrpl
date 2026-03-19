use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainCreateBridgeTransactor;

impl Transactor for XChainCreateBridgeTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        ctx.tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;

        // SignatureReward is required and must be > 0
        let reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward")
            .ok_or(TransactionResult::TemMalformed)?;
        if reward == 0 {
            return Err(TransactionResult::TemMalformed);
        }

        // Validate bridge spec structure
        let bridge = ctx.tx.get("XChainBridge").unwrap();
        bridge_helpers::serialize_bridge_spec(bridge)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Account must be either LockingChainDoor or IssuingChainDoor
        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let issuing_door = bridge
            .get("IssuingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;

        if account_str != locking_door && account_str != issuing_door {
            return Err(TransactionResult::TecXChainBadDest);
        }

        // Bridge must not already exist
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&account_id, &bridge_data);
        if ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecDuplicate);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let bridge_key = keylet::bridge(&account_id, &bridge_data);

        let signature_reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward")
            .ok_or(TransactionResult::TemMalformed)?;

        // Build the Bridge ledger entry
        let mut entry = serde_json::json!({
            "LedgerEntryType": "Bridge",
            "Account": account_str,
            "XChainBridge": bridge,
            "SignatureReward": signature_reward.to_string(),
            "XChainClaimID": "0",
            "XChainAccountCreateCount": "0",
            "Flags": 0,
        });

        if let Some(min_create) = helpers::get_u64_str_field(ctx.tx, "MinAccountCreateAmount") {
            entry["MinAccountCreateAmount"] = serde_json::Value::String(min_create.to_string());
        }

        let entry_data = serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(bridge_key, entry_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update source account: increment owner count and sequence
        let src_key = keylet::account(&account_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        helpers::adjust_owner_count(&mut src_account, 1);
        helpers::increment_sequence(&mut src_account);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
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

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    fn setup_ledger() -> Ledger {
        let mut ledger = Ledger::genesis();
        for addr in [DOOR, USER] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": "100000000",
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
    fn preflight_missing_bridge() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": DOOR,
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
            XChainCreateBridgeTransactor.preflight(&ctx),
            Err(TransactionResult::TemXChainBridge)
        );
    }

    #[test]
    fn preflight_missing_signature_reward() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
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
            XChainCreateBridgeTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_zero_signature_reward() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "0",
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
            XChainCreateBridgeTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_account_not_door() {
        let ledger = setup_ledger();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        // Use a third account that is not a door
        let other = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
        let other_id = decode_account_id(other).unwrap();
        let mut ledger2 = ledger;
        let other_key = keylet::account(&other_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": other,
            "Balance": "100000000",
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        ledger2
            .put_state(other_key, serde_json::to_vec(&account).unwrap())
            .unwrap();
        let view = LedgerView::with_fees(&ledger2, FeeSettings::default());

        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": other,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCreateBridgeTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainBadDest)
        );
    }

    #[test]
    fn apply_creates_bridge() {
        let ledger = setup_ledger();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "MinAccountCreateAmount": "10000000",
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainCreateBridgeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify bridge entry exists
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let entry_bytes = sandbox.read(&bridge_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["LedgerEntryType"].as_str().unwrap(), "Bridge");
        assert_eq!(entry["SignatureReward"].as_str().unwrap(), "100");
        assert_eq!(entry["XChainClaimID"].as_str().unwrap(), "0");

        // Verify owner count incremented
        let src_key = keylet::account(&door_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn preclaim_duplicate_bridge() {
        let mut ledger = setup_ledger();
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let entry = serde_json::json!({
            "LedgerEntryType": "Bridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "XChainClaimID": "0",
            "XChainAccountCreateCount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(bridge_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();

        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "XChainCreateBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCreateBridgeTransactor.preclaim(&ctx),
            Err(TransactionResult::TecDuplicate)
        );
    }
}
