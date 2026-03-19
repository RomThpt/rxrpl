use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainClaimTransactor;

impl Transactor for XChainClaimTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        let bridge = ctx
            .tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;
        bridge_helpers::serialize_bridge_spec(bridge)?;

        // XChainClaimID is required
        helpers::get_u64_str_field(ctx.tx, "XChainClaimID")
            .ok_or(TransactionResult::TemMalformed)?;

        // Destination is required
        helpers::get_destination(ctx.tx)?;

        // Amount is required and must be > 0
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;

        // Bridge must exist
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let door_id =
            decode_account_id(locking_door).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Claim ID entry must exist
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        if !ctx.view.exists(&claim_key) {
            return Err(TransactionResult::TecXChainNoClaimId);
        }

        // Simplified quorum check: at least 1 attestation
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let attestations = claim_entry
            .get("Attestations")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if attestations == 0 {
            return Err(TransactionResult::TecXChainClaimNoQuorum);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let destination_str = helpers::get_destination(ctx.tx)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();

        // Read claim entry to get the original creator for owner count
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let creator_str = claim_entry["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        // Credit Amount to Destination's XRP balance
        let dest_id = decode_account_id(destination_str)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let dest_key = keylet::account(&dest_id);
        let dest_bytes = ctx
            .view
            .read(&dest_key)
            .ok_or(TransactionResult::TecXChainNoDst)?;
        let mut dest_account: serde_json::Value =
            serde_json::from_slice(&dest_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let dest_balance = helpers::get_balance(&dest_account);
        helpers::set_balance(&mut dest_account, dest_balance + amount);

        let dest_data =
            serde_json::to_vec(&dest_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(dest_key, dest_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Erase claim ID entry
        ctx.view
            .erase(&claim_key)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Decrement owner count on the original creator
        let creator_id =
            decode_account_id(&creator_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let creator_key = keylet::account(&creator_id);
        let creator_bytes = ctx
            .view
            .read(&creator_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut creator_account: serde_json::Value =
            serde_json::from_slice(&creator_bytes).map_err(|_| TransactionResult::TefInternal)?;
        helpers::adjust_owner_count(&mut creator_account, -1);

        let creator_data =
            serde_json::to_vec(&creator_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(creator_key, creator_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Increment sender sequence
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
    const DEST: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    fn setup_with_attested_claim() -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, balance) in [
            (DOOR, 100_000_000u64),
            (USER, 100_000_000),
            (DEST, 50_000_000),
        ] {
            let id = decode_account_id(addr).unwrap();
            let key = keylet::account(&id);
            let account = serde_json::json!({
                "LedgerEntryType": "AccountRoot",
                "Account": addr,
                "Balance": balance.to_string(),
                "Sequence": 1,
                "OwnerCount": 1,
                "Flags": 0,
            });
            ledger
                .put_state(key, serde_json::to_vec(&account).unwrap())
                .unwrap();
        }

        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let bridge_entry = serde_json::json!({
            "LedgerEntryType": "Bridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "XChainClaimID": "1",
            "XChainAccountCreateCount": "0",
            "Flags": 0,
        });
        ledger
            .put_state(bridge_key, serde_json::to_vec(&bridge_entry).unwrap())
            .unwrap();

        // Create claim ID entry with an attestation
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedClaimID",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "OtherChainSource": DOOR,
            "Attestations": [{
                "AttestationSignerAccount": DOOR,
                "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
                "Amount": "10000000",
                "AttestationRewardAccount": DOOR,
                "WasLockingChainSend": 1
            }],
            "CommitAmount": "10000000",
            "SendingAccount": DEST,
            "Flags": 0,
        });
        ledger
            .put_state(claim_key, serde_json::to_vec(&claim_entry).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_destination() {
        let tx = serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Amount": "10000000",
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
            XChainClaimTransactor.preflight(&ctx),
            Err(TransactionResult::TemDstIsObligatory)
        );
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "0",
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
            XChainClaimTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn preclaim_no_quorum() {
        let mut ledger = setup_with_attested_claim();
        // Replace claim with one that has no attestations
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedClaimID",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Attestations": [],
            "Flags": 0,
        });
        ledger
            .put_state(claim_key, serde_json::to_vec(&claim_entry).unwrap())
            .unwrap();

        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();
        let tx = serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "10000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainClaimTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainClaimNoQuorum)
        );
    }

    #[test]
    fn apply_claims_funds() {
        let ledger = setup_with_attested_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "10000000",
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainClaimTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify destination balance increased
        let dest_id = decode_account_id(DEST).unwrap();
        let dest_key = keylet::account(&dest_id);
        let dest_bytes = sandbox.read(&dest_key).unwrap();
        let dest: serde_json::Value = serde_json::from_slice(&dest_bytes).unwrap();
        assert_eq!(dest["Balance"].as_str().unwrap(), "60000000");

        // Verify claim ID entry erased
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        assert!(sandbox.read(&claim_key).is_none());

        // Verify creator owner count decremented
        let user_id = decode_account_id(USER).unwrap();
        let creator_key = keylet::account(&user_id);
        let creator_bytes = sandbox.read(&creator_key).unwrap();
        let creator: serde_json::Value = serde_json::from_slice(&creator_bytes).unwrap();
        assert_eq!(creator["OwnerCount"].as_u64().unwrap(), 0);
    }

    #[test]
    fn preclaim_no_claim_id() {
        let ledger = setup_with_attested_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "999",
            "Destination": DEST,
            "Amount": "10000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainClaimTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainNoClaimId)
        );
    }
}
