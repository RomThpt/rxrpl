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

        // Bridge must exist (on either chain's door).
        let bridge_key = bridge_helpers::find_bridge_keylet(bridge, |k| ctx.view.exists(k))?;
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Destination must exist.
        let dst = helpers::get_destination(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, dst).map_err(|_| TransactionResult::TecNoDst)?;

        // Claim ID entry must exist and be owned by the submitter.
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;
        if claim_entry["Account"].as_str() != Some(account_str) {
            return Err(TransactionResult::TecXChainBadClaimId);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?.to_string();
        let account_id =
            decode_account_id(&account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let destination_str = helpers::get_destination(ctx.tx)?.to_string();
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();

        // The bridge SLE's Account is the door that pays on this chain. The source
        // chain is the opposite of the door's chain: a locking-chain door means the
        // funds were sent on the issuing chain (wasLockingChainSend = 0), and a
        // door on the issuing chain means wasLockingChainSend = 1.
        let bridge_key = bridge_helpers::find_bridge_keylet(&bridge, |k| ctx.view.exists(k))?;
        let bridge_sle: serde_json::Value = serde_json::from_slice(
            &ctx.view.read(&bridge_key).ok_or(TransactionResult::TecNoEntry)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let door_str = bridge_sle["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let door_is_locking = bridge["LockingChainDoor"].as_str() == Some(door_str.as_str());
        let was_locking = u64::from(!door_is_locking);

        let (signers, quorum) =
            crate::xchain_attestation::read_signers_and_quorum(ctx.view, &door_str)?;

        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_entry: serde_json::Value = serde_json::from_slice(
            &ctx.view
                .read(&claim_key)
                .ok_or(TransactionResult::TecXChainNoClaimId)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let claim_owner = claim_entry["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        // Quorum over the stored attestations matching this amount + send direction,
        // ignoring destination (onClaim / CheckDst::Ignore).
        let mut weight: u64 = 0;
        let mut reward_accounts: Vec<String> = Vec::new();
        if let Some(atts) = claim_entry
            .get("XChainClaimAttestations")
            .and_then(|v| v.as_array())
        {
            for e in atts {
                let a = e.get("XChainClaimProofSig").unwrap_or(e);
                let same = a["Amount"].as_str() == Some(amount.to_string().as_str())
                    && a["WasLockingChainSend"].as_u64() == Some(was_locking);
                if !same {
                    continue;
                }
                if let Some(w) = a["AttestationSignerAccount"]
                    .as_str()
                    .and_then(|sa| signers.get(sa))
                {
                    weight += *w;
                    if let Some(ra) = a["AttestationRewardAccount"].as_str() {
                        reward_accounts.push(ra.to_string());
                    }
                }
            }
        }
        if weight < quorum {
            return Err(TransactionResult::TecXChainClaimNoQuorum);
        }

        let reward_pool = claim_entry["SignatureReward"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        crate::xchain_attestation::finalize_claim_xrp(
            ctx.view,
            &door_str,
            &destination_str,
            &claim_owner,
            amount,
            reward_pool,
            &reward_accounts,
            &claim_key,
        )?;

        // Increment sender sequence.
        let src_key = keylet::account(&account_id);
        let mut src_account: serde_json::Value = serde_json::from_slice(
            &ctx.view.read(&src_key).ok_or(TransactionResult::TerNoAccount)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        helpers::increment_sequence(&mut src_account);
        ctx.view
            .update(
                src_key,
                serde_json::to_vec(&src_account).map_err(|_| TransactionResult::TefInternal)?,
            )
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
    const WITNESS: &str = "r4nvJ7S4fsLpRPKPTLYsqpE4dZ8XHXh57e";
    const DEST: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    /// Bridge whose paying door (DOOR, the locking-chain door) carries a
    /// quorum-1 witness signer list, plus a claim id (id 1) owned by USER that
    /// already holds `n` quorum-meeting attestations (WasLockingChainSend = 0).
    fn setup(with_attestation: bool) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, oc, bal) in [
            (DOOR, 0u64, 100_000_000u64),
            (USER, 1, 100_000_000),
            (WITNESS, 0, 100_000_000),
            (DEST, 0, 50_000_000),
        ] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot",
                        "Account": addr,
                        "Balance": bal.to_string(),
                        "Sequence": 1,
                        "OwnerCount": oc,
                        "Flags": 0,
                    }))
                    .unwrap(),
                )
                .unwrap();
        }

        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_key = bridge_helpers::bridge_keylet_for_door(
            &door_id,
            bridge_spec().get("LockingChainIssue").unwrap(),
        );
        ledger
            .put_state(
                bridge_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "Bridge",
                    "Account": DOOR,
                    "XChainBridge": bridge_spec(),
                    "SignatureReward": "100",
                    "XChainClaimID": "1",
                    "XChainAccountCreateCount": "0",
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();
        ledger
            .put_state(
                keylet::signer_list(&door_id),
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "SignerList",
                    "Account": DOOR,
                    "SignerQuorum": 1,
                    "SignerEntries": [
                        {"SignerEntry": {"Account": WITNESS, "SignerWeight": 1}},
                    ],
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let atts = if with_attestation {
            serde_json::json!([{
                "XChainClaimProofSig": {
                    "AttestationSignerAccount": WITNESS,
                    "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
                    "Amount": "10000000",
                    "AttestationRewardAccount": WITNESS,
                    "WasLockingChainSend": 0
                }
            }])
        } else {
            serde_json::json!([])
        };
        ledger
            .put_state(
                claim_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "XChainOwnedClaimID",
                    "Account": USER,
                    "XChainBridge": bridge_spec(),
                    "XChainClaimID": "1",
                    "OtherChainSource": WITNESS,
                    "SignatureReward": "100",
                    "XChainClaimAttestations": atts,
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        ledger
    }

    fn claim_tx(claim_id: &str) -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "XChainClaim",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": claim_id,
            "Destination": DEST,
            "Amount": "10000000",
            "Fee": "12",
        })
    }

    #[test]
    fn preflight_missing_destination() {
        let mut tx = claim_tx("1");
        tx.as_object_mut().unwrap().remove("Destination");
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
        let mut tx = claim_tx("1");
        tx["Amount"] = serde_json::Value::String("0".into());
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
    fn apply_no_quorum_rejected() {
        let ledger = setup(false);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = claim_tx("1");
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainClaimTransactor.apply(&mut ctx),
            Err(TransactionResult::TecXChainClaimNoQuorum)
        );
    }

    #[test]
    fn apply_claims_funds() {
        let ledger = setup(true);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = claim_tx("1");
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainClaimTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let read = |addr: &str| -> serde_json::Value {
            let id = decode_account_id(addr).unwrap();
            serde_json::from_slice(&sandbox.read(&keylet::account(&id)).unwrap()).unwrap()
        };
        assert_eq!(read(DOOR)["Balance"].as_str().unwrap(), "90000000");
        assert_eq!(read(DEST)["Balance"].as_str().unwrap(), "60000000");
        assert_eq!(read(WITNESS)["Balance"].as_str().unwrap(), "100000100");
        assert_eq!(read(USER)["Balance"].as_str().unwrap(), "99999900");
        assert_eq!(read(USER)["OwnerCount"].as_u64().unwrap(), 0);

        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        assert!(sandbox.read(&claim_key).is_none());
    }

    #[test]
    fn preclaim_no_claim_id() {
        let ledger = setup(true);
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = claim_tx("999");
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
