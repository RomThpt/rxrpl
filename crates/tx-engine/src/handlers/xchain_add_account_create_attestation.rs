use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainAddAccountCreateAttestationTransactor;

impl Transactor for XChainAddAccountCreateAttestationTransactor {
    fn preflight(&self, ctx: &PreflightContext<'_>) -> Result<(), TransactionResult> {
        // XChainBridge is required
        let bridge = ctx
            .tx
            .get("XChainBridge")
            .ok_or(TransactionResult::TemXChainBridge)?;
        bridge_helpers::serialize_bridge_spec(bridge)?;

        // XChainAccountCreateCount is required
        helpers::get_u64_str_field(ctx.tx, "XChainAccountCreateCount")
            .ok_or(TransactionResult::TemMalformed)?;

        // Destination is required
        helpers::get_destination(ctx.tx)?;

        // Amount is required
        helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

        // SignatureReward is required
        helpers::get_u64_str_field(ctx.tx, "SignatureReward")
            .ok_or(TransactionResult::TemMalformed)?;

        // Attestation fields are required
        helpers::get_str_field(ctx.tx, "AttestationSignerAccount")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "PublicKey").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "Signature").ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "AttestationRewardAccount")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_u32_field(ctx.tx, "WasLockingChainSend")
            .ok_or(TransactionResult::TemMalformed)?;

        Ok(())
    }

    fn preclaim(&self, ctx: &PreclaimContext<'_>) -> Result<(), TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        helpers::read_account_by_address(ctx.view, account_str)?;

        // Bridge must exist (on either chain's door). The create-account claim id
        // object is created by this very transaction, so it is not required here.
        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_key = bridge_helpers::find_bridge_keylet(bridge, |k| ctx.view.exists(k))?;
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?.to_string();
        let account_id =
            decode_account_id(&account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let create_count = helpers::get_u64_str_field(ctx.tx, "XChainAccountCreateCount").unwrap();

        let attestation_signer = helpers::get_str_field(ctx.tx, "AttestationSignerAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let public_key = helpers::get_str_field(ctx.tx, "PublicKey")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward = helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);
        let reward_account = helpers::get_str_field(ctx.tx, "AttestationRewardAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let was_locking = helpers::get_u32_field(ctx.tx, "WasLockingChainSend")
            .ok_or(TransactionResult::TemMalformed)?;
        let dst = helpers::get_destination(ctx.tx)?.to_string();

        // Locate the bridge SLE; its Account is the door that funds the new account
        // and the reward pool.
        let bridge_key = bridge_helpers::find_bridge_keylet(&bridge, |k| ctx.view.exists(k))?;
        let mut bridge_sle: serde_json::Value = serde_json::from_slice(
            &ctx.view.read(&bridge_key).ok_or(TransactionResult::TecNoEntry)?,
        )
        .map_err(|_| TransactionResult::TefInternal)?;
        let door_str = bridge_sle["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();
        let claim_count: u64 = bridge_sle["XChainAccountClaimCount"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // The create count must be in the future relative to the door's processed
        // count (claimHelper ordering).
        if create_count <= claim_count {
            return Err(TransactionResult::TecXChainAccountCreatePastSeq);
        }

        let (signers, quorum) =
            crate::xchain_attestation::read_signers_and_quorum(ctx.view, &door_str)?;
        if !signers.contains_key(&attestation_signer) {
            return Err(TransactionResult::TecXChainProofUnknownKey);
        }

        // Stored attestation element (canonical create-account SLE form).
        let element = serde_json::json!({ "XChainCreateAccountProofSig": {
            "AttestationSignerAccount": attestation_signer,
            "PublicKey": public_key,
            "Amount": amount.to_string(),
            "SignatureReward": sig_reward.to_string(),
            "AttestationRewardAccount": reward_account,
            "WasLockingChainSend": was_locking,
            "Destination": dst,
        }});

        let create_key = keylet::xchain_create_account_claim_id(&bridge_data, create_count);
        let mut atts: Vec<serde_json::Value> = ctx
            .view
            .read(&create_key)
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| {
                v.get("XChainCreateAccountAttestations")
                    .and_then(|a| a.as_array())
                    .cloned()
            })
            .unwrap_or_default();
        let inner = |e: &serde_json::Value| -> serde_json::Value {
            e.get("XChainCreateAccountProofSig").cloned().unwrap_or_else(|| e.clone())
        };
        if let Some(slot) = atts.iter_mut().find(|e| {
            inner(e)["AttestationSignerAccount"].as_str() == Some(attestation_signer.as_str())
        }) {
            *slot = element;
        } else {
            atts.push(element);
        }

        // Weighted quorum over attestations matching amount + reward + direction + dst.
        let mut weight: u64 = 0;
        let mut reward_accounts: Vec<String> = Vec::new();
        for e in &atts {
            let a = inner(e);
            let same = a["Amount"].as_str() == Some(amount.to_string().as_str())
                && a["SignatureReward"].as_str() == Some(sig_reward.to_string().as_str())
                && a["WasLockingChainSend"].as_u64() == Some(u64::from(was_locking))
                && a["Destination"].as_str() == Some(dst.as_str());
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

        if weight >= quorum && claim_count + 1 == create_count {
            let ledger_seq = ctx.view.seq();
            crate::xchain_attestation::finalize_create_account_xrp(
                ctx.view,
                &door_str,
                &dst,
                amount,
                sig_reward,
                &reward_accounts,
                ledger_seq,
            )?;
            // Advance the door's processed-create counter; a never-created object
            // is left behind (transient create + delete).
            bridge_sle["XChainAccountClaimCount"] =
                serde_json::Value::String(create_count.to_string());
            ctx.view
                .update(
                    bridge_key,
                    serde_json::to_vec(&bridge_sle)
                        .map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        } else if ctx.view.read(&create_key).is_none() {
            // No quorum yet: create the door-owned create-account claim id object.
            let door_id =
                decode_account_id(&door_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;
            let entry = serde_json::json!({
                "LedgerEntryType": "XChainOwnedCreateAccountClaimID",
                "Account": door_str,
                "XChainBridge": bridge,
                "XChainAccountCreateCount": create_count.to_string(),
                "XChainCreateAccountAttestations": atts,
                "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
                "PreviousTxnLgrSeq": 0,
            });
            ctx.view
                .insert(
                    create_key,
                    serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
            crate::owner_dir::add_to_owner_dir(ctx.view, &door_id, &create_key)?;
            let door_key = keylet::account(&door_id);
            let mut door_acct: serde_json::Value = serde_json::from_slice(
                &ctx.view.read(&door_key).ok_or(TransactionResult::TerNoAccount)?,
            )
            .map_err(|_| TransactionResult::TefInternal)?;
            helpers::adjust_owner_count(&mut door_acct, 1);
            ctx.view
                .update(
                    door_key,
                    serde_json::to_vec(&door_acct).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        } else {
            // Existing object below quorum: store the merged attestations.
            let mut entry: serde_json::Value = serde_json::from_slice(
                &ctx.view.read(&create_key).unwrap(),
            )
            .map_err(|_| TransactionResult::TefInternal)?;
            entry["XChainCreateAccountAttestations"] = serde_json::Value::Array(atts);
            ctx.view
                .update(
                    create_key,
                    serde_json::to_vec(&entry).map_err(|_| TransactionResult::TefInternal)?,
                )
                .map_err(|_| TransactionResult::TefInternal)?;
        }

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
    use crate::transactor::{ApplyContext, PreflightContext};
    use crate::view::ledger_view::LedgerView;
    use crate::view::read_view::ReadView;
    use crate::view::sandbox::Sandbox;
    use rxrpl_amendment::Rules;
    use rxrpl_ledger::Ledger;

    const DOOR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
    const USER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";
    const WITNESS: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
    const NEW_ACCOUNT: &str = "rh5RDbBZvsRixPtyjFTj4CGndCvv6T2Y1S";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    /// Bridge whose paying door (DOOR) carries a quorum-`quorum` witness signer
    /// list and an unconsumed account-claim counter (XChainAccountClaimCount = 0).
    /// The destination account (NEW_ACCOUNT) does not exist yet.
    fn setup(quorum: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        for addr in [DOOR, USER, WITNESS] {
            let id = decode_account_id(addr).unwrap();
            ledger
                .put_state(
                    keylet::account(&id),
                    serde_json::to_vec(&serde_json::json!({
                        "LedgerEntryType": "AccountRoot",
                        "Account": addr,
                        "Balance": "1000000000",
                        "Sequence": 1,
                        "OwnerCount": 0,
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
                    "MinAccountCreateAmount": "5000000",
                    "XChainClaimID": "0",
                    "XChainAccountCreateCount": "0",
                    "XChainAccountClaimCount": "0",
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
                    "SignerQuorum": quorum,
                    "SignerEntries": [
                        {"SignerEntry": {"Account": WITNESS, "SignerWeight": 1}},
                    ],
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        ledger
    }

    fn attestation_tx(count: &str) -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": count,
            "Destination": NEW_ACCOUNT,
            "Amount": "20000000",
            "SignatureReward": "100",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
            "Fee": "12",
        })
    }

    #[test]
    fn preflight_missing_count() {
        let mut tx = attestation_tx("1");
        tx.as_object_mut().unwrap().remove("XChainAccountCreateCount");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddAccountCreateAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_signature() {
        let mut tx = attestation_tx("1");
        tx.as_object_mut().unwrap().remove("Signature");
        let rules = Rules::new();
        let fees = FeeSettings::default();
        let ctx = PreflightContext {
            tx: &tx,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddAccountCreateAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn apply_past_seq_rejected() {
        let ledger = setup(1);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        // create_count 0 is not > the door's claim count (0).
        let tx = attestation_tx("0");
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddAccountCreateAttestationTransactor.apply(&mut ctx),
            Err(TransactionResult::TecXChainAccountCreatePastSeq)
        );
    }

    #[test]
    fn apply_quorum_creates_account() {
        let ledger = setup(1);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = attestation_tx("1");
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddAccountCreateAttestationTransactor
                .apply(&mut ctx)
                .unwrap(),
            TransactionResult::TesSuccess
        );

        let read = |addr: &str| -> serde_json::Value {
            let id = decode_account_id(addr).unwrap();
            serde_json::from_slice(&sandbox.read(&keylet::account(&id)).unwrap()).unwrap()
        };
        // Door funds the new account (20000000) and the reward (100).
        assert_eq!(read(DOOR)["Balance"].as_str().unwrap(), "979999900");
        assert_eq!(read(NEW_ACCOUNT)["Balance"].as_str().unwrap(), "20000000");
        assert_eq!(read(WITNESS)["Balance"].as_str().unwrap(), "1000000100");

        // Bridge counter advanced; no claim-id object left behind.
        let door_id = decode_account_id(DOOR).unwrap();
        let bridge_key = bridge_helpers::bridge_keylet_for_door(
            &door_id,
            bridge_spec().get("LockingChainIssue").unwrap(),
        );
        let bridge: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&bridge_key).unwrap()).unwrap();
        assert_eq!(bridge["XChainAccountClaimCount"].as_str().unwrap(), "1");
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let create_key = keylet::xchain_create_account_claim_id(&bridge_data, 1);
        assert!(sandbox.read(&create_key).is_none());
    }

    #[test]
    fn apply_below_quorum_persists_claim_id() {
        let ledger = setup(2);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = attestation_tx("1");
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        XChainAddAccountCreateAttestationTransactor
            .apply(&mut ctx)
            .unwrap();

        // Quorum (2) not reached: door-owned claim-id object holds the attestation.
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let create_key = keylet::xchain_create_account_claim_id(&bridge_data, 1);
        let entry: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&create_key).unwrap()).unwrap();
        assert_eq!(entry["Account"].as_str().unwrap(), DOOR);
        let atts = entry["XChainCreateAccountAttestations"].as_array().unwrap();
        assert_eq!(atts.len(), 1);

        // Account not created; door owner count incremented.
        let new_id = decode_account_id(NEW_ACCOUNT).unwrap();
        assert!(sandbox.read(&keylet::account(&new_id)).is_none());
        let door_id = decode_account_id(DOOR).unwrap();
        let door: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&keylet::account(&door_id)).unwrap()).unwrap();
        assert_eq!(door["OwnerCount"].as_u64().unwrap(), 1);
    }
}
