use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainCommitTransactor;

impl Transactor for XChainCommitTransactor {
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

        // Amount is required and must be > 0
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        if amount == 0 {
            return Err(TransactionResult::TemBadAmount);
        }

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let (_, src_account) = helpers::read_account_by_address(ctx.view, account_str)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;

        // Account must not be a door account (no self-commit)
        let locking_door = bridge
            .get("LockingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        let issuing_door = bridge
            .get("IssuingChainDoor")
            .and_then(|v| v.as_str())
            .ok_or(TransactionResult::TemXChainBridge)?;
        if account_str == locking_door || account_str == issuing_door {
            return Err(TransactionResult::TecXChainSelfCommit);
        }

        // Bridge must exist
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

        // Check sufficient balance
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let fee = helpers::get_fee(ctx.tx);
        let balance = helpers::get_balance(&src_account);
        if balance < amount + fee {
            return Err(TransactionResult::TecUnfundedPayment);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();

        // Deduct Amount from sender's XRP balance
        let src_key = keylet::account(&account_id);
        let src_bytes = ctx
            .view
            .read(&src_key)
            .ok_or(TransactionResult::TerNoAccount)?;
        let mut src_account: serde_json::Value =
            serde_json::from_slice(&src_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let src_balance = helpers::get_balance(&src_account);
        helpers::set_balance(
            &mut src_account,
            src_balance
                .checked_sub(amount)
                .ok_or(TransactionResult::TecUnfundedPayment)?,
        );
        helpers::increment_sequence(&mut src_account);

        let src_data =
            serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(src_key, src_data)
            .map_err(|_| TransactionResult::TefInternal)?;

        // Update claim ID entry with commitment
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let mut claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;

        claim_entry["CommitAmount"] = serde_json::Value::String(amount.to_string());
        claim_entry["SendingAccount"] = serde_json::Value::String(account_str.to_string());

        if let Some(dest) = helpers::get_str_field(ctx.tx, "OtherChainDestination") {
            claim_entry["OtherChainDestination"] = serde_json::Value::String(dest.to_string());
        }

        let claim_data =
            serde_json::to_vec(&claim_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(claim_key, claim_data)
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

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    fn setup_with_bridge_and_claim() -> Ledger {
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

        // Create claim ID entry
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedClaimID",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "OtherChainSource": SENDER,
            "Attestations": [],
            "Flags": 0,
        });
        ledger
            .put_state(claim_key, serde_json::to_vec(&claim_entry).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_amount() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
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
            XChainCommitTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn preflight_zero_amount() {
        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
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
            XChainCommitTransactor.preflight(&ctx),
            Err(TransactionResult::TemBadAmount)
        );
    }

    #[test]
    fn preclaim_self_commit_rejected() {
        let ledger = setup_with_bridge_and_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Amount": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCommitTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainSelfCommit)
        );
    }

    #[test]
    fn preclaim_no_claim_id() {
        let ledger = setup_with_bridge_and_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "999",
            "Amount": "1000000",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCommitTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainNoClaimId)
        );
    }

    #[test]
    fn apply_commits_funds() {
        let ledger = setup_with_bridge_and_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Amount": "5000000",
            "OtherChainDestination": USER,
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainCommitTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify sender balance decreased
        let sender_id = decode_account_id(SENDER).unwrap();
        let src_key = keylet::account(&sender_id);
        let src_bytes = sandbox.read(&src_key).unwrap();
        let src: serde_json::Value = serde_json::from_slice(&src_bytes).unwrap();
        assert_eq!(src["Balance"].as_str().unwrap(), "45000000");

        // Verify claim ID entry updated with commitment
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_bytes = sandbox.read(&claim_key).unwrap();
        let claim: serde_json::Value = serde_json::from_slice(&claim_bytes).unwrap();
        assert_eq!(claim["CommitAmount"].as_str().unwrap(), "5000000");
        assert_eq!(claim["SendingAccount"].as_str().unwrap(), SENDER);
        assert_eq!(claim["OtherChainDestination"].as_str().unwrap(), USER);
    }

    #[test]
    fn preclaim_insufficient_balance() {
        let ledger = setup_with_bridge_and_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainCommit",
            "Account": SENDER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Amount": "999999999999",
            "Fee": "12",
        });
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainCommitTransactor.preclaim(&ctx),
            Err(TransactionResult::TecUnfundedPayment)
        );
    }
}
