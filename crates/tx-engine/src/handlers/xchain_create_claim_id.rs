use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainCreateClaimIdTransactor;

impl Transactor for XChainCreateClaimIdTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        let bridge = ctx
            .tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;
        bridge_helpers::serialize_bridge_spec(bridge)?;

        // OtherChainSource is required
        helpers::get_str_field(ctx.tx, "OtherChainSource")
            .ok_or(TransactionResult::TemMalformed)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Bridge must exist -- look up by door account
        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;
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

        Ok(())
    }

    fn apply(
        &self,
        ctx: &mut ApplyContext<'_>,
    ) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id = decode_account_id(account_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let other_chain_source = helpers::get_str_field(ctx.tx, "OtherChainSource")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();

        // Read bridge entry and increment XChainClaimID counter
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

        let current_claim_id: u64 = bridge_entry["XChainClaimID"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let new_claim_id = current_claim_id + 1;
        bridge_entry["XChainClaimID"] =
            serde_json::Value::String(new_claim_id.to_string());

        let bridge_data_updated =
            serde_json::to_vec(&bridge_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(bridge_key, bridge_data_updated)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Create XChainOwnedClaimID entry
        let claim_key = keylet::xchain_claim_id(&bridge_data, new_claim_id);
        let claim_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedClaimID",
            "Account": account_str,
            "XChainBridge": bridge,
            "XChainClaimID": new_claim_id.to_string(),
            "OtherChainSource": other_chain_source,
            "Attestations": [],
            "Flags": 0,
        });

        let claim_data =
            serde_json::to_vec(&claim_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .insert(claim_key, claim_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update creator account: increment owner count and sequence
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
            "TransactionType": "XChainCreateClaimId",
            "Account": USER,
            "OtherChainSource": DOOR,
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
            XChainCreateClaimIdTransactor.preflight(&ctx),
            Err(TransactionResult::TemXChainBridge)
        );
    }

    #[test]
    fn preflight_missing_other_chain_source() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCreateClaimId",
            "Account": USER,
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
            XChainCreateClaimIdTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_no_bridge() {
        let mut ledger = Ledger::genesis();
        let user_id = decode_account_id(USER).unwrap();
        let key = keylet::account(&user_id);
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": USER,
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
            "TransactionType": "XChainCreateClaimId",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "OtherChainSource": DOOR,
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCreateClaimIdTransactor.preclaim(&ctx),
            Err(TransactionResult::TecNoEntry)
        );
    }

    #[test]
    fn apply_creates_claim_id() {
        let ledger = setup_with_bridge();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCreateClaimId",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "OtherChainSource": DOOR,
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainCreateClaimIdTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify claim ID entry was created
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_bytes = sandbox.read(&claim_key).unwrap();
        let claim: serde_json::Value = serde_json::from_slice(&claim_bytes).unwrap();
        assert_eq!(
            claim["LedgerEntryType"].as_str().unwrap(),
            "XChainOwnedClaimID"
        );
        assert_eq!(claim["XChainClaimID"].as_str().unwrap(), "1");
        assert_eq!(claim["Account"].as_str().unwrap(), USER);

        // Verify bridge counter incremented
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let bridge_bytes = sandbox.read(&bridge_key).unwrap();
        let bridge_entry: serde_json::Value =
            serde_json::from_slice(&bridge_bytes).unwrap();
        assert_eq!(bridge_entry["XChainClaimID"].as_str().unwrap(), "1");

        // Verify owner count incremented
        let user_id = decode_account_id(USER).unwrap();
        let src_key = keylet::account(&user_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["OwnerCount"].as_u64().unwrap(), 1);
    }

    #[test]
    fn apply_increments_claim_id_counter() {
        let ledger = setup_with_bridge();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCreateClaimId",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "OtherChainSource": DOOR,
            "Fee": "12",
        });

        // First claim ID
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        XChainCreateClaimIdTransactor.apply(&mut ctx).unwrap();

        // Second claim ID
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        let result = XChainCreateClaimIdTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify second claim ID entry
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 2);
        let claim_bytes = sandbox.read(&claim_key).unwrap();
        let claim: serde_json::Value = serde_json::from_slice(&claim_bytes).unwrap();
        assert_eq!(claim["XChainClaimID"].as_str().unwrap(), "2");
    }
}
