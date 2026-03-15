use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainModifyBridgeTransactor;

impl Transactor for XChainModifyBridgeTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        let bridge = ctx
            .tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;
        bridge_helpers::serialize_bridge_spec(bridge)?;

        // At least one of SignatureReward or MinAccountCreateAmount must be present
        let has_reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward").is_some();
        let has_min_create =
            helpers::get_u64_str_field(ctx.tx, "MinAccountCreateAmount").is_some();
        if !has_reward && !has_min_create {
            return Err(TransactionResult::TemMalformed);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let issuing_door = bridge
            .get("IssuingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;

        // Account must be a door account
        if account_str != locking_door && account_str != issuing_door {
            return Err(TransactionResult::TecXChainBadDest);
        }

        // Bridge must exist
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&account_id, &bridge_data);
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
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

        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;
        let bridge_key = keylet::bridge(&account_id, &bridge_data);

        // Read existing bridge entry
        let entry_bytes = ctx
            .view
            .read(&bridge_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let mut entry: serde_json::Value =
            serde_json::from_slice(&entry_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // Update fields
        if let Some(reward) = helpers::get_u64_str_field(ctx.tx, "SignatureReward") {
            entry["SignatureReward"] = serde_json::Value::String(reward.to_string());
        }
        if let Some(min_create) = helpers::get_u64_str_field(ctx.tx, "MinAccountCreateAmount") {
            entry["MinAccountCreateAmount"] =
                serde_json::Value::String(min_create.to_string());
        }

        let entry_data =
            serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(bridge_key, entry_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update source account sequence
        let src_key = keylet::account(&account_id);
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

    fn setup_with_bridge() -> Ledger {
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

        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let entry = serde_json::json!({
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
            .put_state(bridge_key, serde_json::to_vec(&entry).unwrap())
            .unwrap();
        ledger
    }

    #[test]
    fn preflight_missing_bridge() {
        let tx = serde_json::json!({
            "TransactionType": "XChainModifyBridge",
            "Account": DOOR,
            "SignatureReward": "200",
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
            XChainModifyBridgeTransactor.preflight(&ctx),
            Err(TransactionResult::TemXChainBridge)
        );
    }

    #[test]
    fn preflight_no_update_fields() {
        let tx = serde_json::json!({
            "TransactionType": "XChainModifyBridge",
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
            XChainModifyBridgeTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_bridge_not_found() {
        let mut ledger = Ledger::genesis();
        let door_id = decode_account_id(DOOR).unwrap();
        let key = keylet::account(&door_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": DOOR,
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
            "TransactionType": "XChainModifyBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "200",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainModifyBridgeTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_updates_signature_reward() {
        let ledger = setup_with_bridge();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainModifyBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "500",
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainModifyBridgeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify updated
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let entry_bytes = sandbox.read(&bridge_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(entry["SignatureReward"].as_str().unwrap(), "500");
    }

    #[test]
    fn apply_updates_min_account_create_amount() {
        let ledger = setup_with_bridge();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainModifyBridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "MinAccountCreateAmount": "20000000",
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainModifyBridgeTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let entry_bytes = sandbox.read(&bridge_key).unwrap();
        let entry: serde_json::Value = serde_json::from_slice(&entry_bytes).unwrap();
        assert_eq!(
            entry["MinAccountCreateAmount"].as_str().unwrap(),
            "20000000"
        );
    }
}
