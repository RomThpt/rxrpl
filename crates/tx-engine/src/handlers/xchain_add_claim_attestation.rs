use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{TransactionResult, keylet};

use crate::bridge_helpers;
use crate::helpers;
use crate::transactor::{ApplyContext, PreclaimContext, PreflightContext, Transactor};

pub struct XChainAddClaimAttestationTransactor;

impl Transactor for XChainAddClaimAttestationTransactor {
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

        // Amount is required
        helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;

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

        let bridge = ctx.tx.get("XChainBridge").unwrap();
        let bridge_data = bridge_helpers::serialize_bridge_spec(bridge)?;

        // Bridge must exist (on either chain's door).
        let bridge_key = bridge_helpers::find_bridge_keylet(bridge, |k| ctx.view.exists(k))?;
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Claim ID entry must exist
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        if !ctx.view.exists(&claim_key) {
            return Err(TransactionResult::TecXChainNoClaimId);
        }

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?.to_string();
        let account_id =
            decode_account_id(&account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

        let bridge = ctx.tx.get("XChainBridge").unwrap().clone();
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge)?;
        let claim_id = helpers::get_u64_str_field(ctx.tx, "XChainClaimID").unwrap();

        let attestation_signer = helpers::get_str_field(ctx.tx, "AttestationSignerAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let public_key = helpers::get_str_field(ctx.tx, "PublicKey")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let amount = helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let reward_account = helpers::get_str_field(ctx.tx, "AttestationRewardAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let was_locking = helpers::get_u32_field(ctx.tx, "WasLockingChainSend")
            .ok_or(TransactionResult::TemMalformed)?;
        let other_chain_source = helpers::get_str_field(ctx.tx, "OtherChainSource")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let dst = helpers::get_str_field(ctx.tx, "Destination").map(|s| s.to_string());

        // Locate the bridge SLE; its Account is the door that pays on this chain.
        let bridge_key = bridge_helpers::find_bridge_keylet(&bridge, |k| ctx.view.exists(k))?;
        let bridge_bytes = ctx
            .view
            .read(&bridge_key)
            .ok_or(TransactionResult::TecNoEntry)?;
        let bridge_sle: serde_json::Value =
            serde_json::from_slice(&bridge_bytes).map_err(|_| TransactionResult::TefInternal)?;
        let door_str = bridge_sle["Account"]
            .as_str()
            .ok_or(TransactionResult::TefInternal)?
            .to_string();

        let (signers, quorum) =
            crate::xchain_attestation::read_signers_and_quorum(ctx.view, &door_str)?;

        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let mut claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;

        // The attestation signer must be a current witness.
        if !signers.contains_key(&attestation_signer) {
            return Err(TransactionResult::TecXChainProofUnknownKey);
        }

        // The sending account on the claim id must match this attestation's source.
        if claim_entry["OtherChainSource"].as_str() != Some(other_chain_source.as_str()) {
            return Err(TransactionResult::TecXChainSendingAccountMismatch);
        }

        // Stored attestation element (no Signature; canonical SLE form).
        let mut proof = serde_json::json!({
            "AttestationSignerAccount": attestation_signer,
            "PublicKey": public_key,
            "Amount": amount.to_string(),
            "AttestationRewardAccount": reward_account,
            "WasLockingChainSend": was_locking,
        });
        if let Some(d) = &dst {
            proof["Destination"] = serde_json::Value::String(d.clone());
        }
        let element = serde_json::json!({ "XChainClaimProofSig": proof });

        // Merge into XChainClaimAttestations: replace any existing element from the
        // same signer, else append (onNewAttestations).
        let mut atts: Vec<serde_json::Value> = claim_entry
            .get("XChainClaimAttestations")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let inner = |e: &serde_json::Value| -> serde_json::Value {
            e.get("XChainClaimProofSig").cloned().unwrap_or_else(|| e.clone())
        };
        if let Some(slot) = atts.iter_mut().find(|e| {
            inner(e)["AttestationSignerAccount"].as_str() == Some(attestation_signer.as_str())
        }) {
            *slot = element;
        } else {
            atts.push(element);
        }

        // Accumulate the weight of every attestation matching this one
        // (amount + wasLockingChainSend + destination), per claimHelper.
        let mut weight: u64 = 0;
        let mut reward_accounts: Vec<String> = Vec::new();
        for e in &atts {
            let a = inner(e);
            let same = a["Amount"].as_str() == Some(amount.to_string().as_str())
                && a["WasLockingChainSend"].as_u64() == Some(u64::from(was_locking))
                && a["Destination"].as_str() == dst.as_deref();
            if !same {
                continue;
            }
            let sa = a["AttestationSignerAccount"].as_str().unwrap_or("");
            if let Some(w) = signers.get(sa) {
                weight += *w;
                if let Some(ra) = a["AttestationRewardAccount"].as_str() {
                    reward_accounts.push(ra.to_string());
                }
            }
        }

        if weight >= quorum && dst.is_some() {
            let reward_pool = claim_entry["SignatureReward"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let claim_owner = claim_entry["Account"]
                .as_str()
                .ok_or(TransactionResult::TefInternal)?
                .to_string();
            crate::xchain_attestation::finalize_claim_xrp(
                ctx.view,
                &door_str,
                dst.as_deref().unwrap(),
                &claim_owner,
                amount,
                reward_pool,
                &reward_accounts,
                &claim_key,
            )?;
        } else {
            claim_entry["XChainClaimAttestations"] = serde_json::Value::Array(atts);
            let claim_data =
                serde_json::to_vec(&claim_entry).map_err(|_| TransactionResult::TefInternal)?;
            ctx.view
                .update(claim_key, claim_data)
                .map_err(|_| TransactionResult::TefInternal)?;
        }

        // Increment sender sequence.
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
    const WITNESS: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
    const WITNESS2: &str = "r4nvJ7S4fsLpRPKPTLYsqpE4dZ8XHXh57e";
    const DEST: &str = "rFkHWQptmkmomUrPtV4efMUpLrPKGnsLS";
    const OCS: &str = "rsTJWwm8yDtGQ9XkZnhRrKZP4L9M6ZfS9x";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    /// Bridge with a quorum-`quorum` witness signer list on the paying door
    /// (DOOR), and a claim id (id 1) owned by USER with the given attestations.
    fn setup(quorum: u64) -> Ledger {
        let mut ledger = Ledger::genesis();
        for (addr, oc, bal) in [
            (DOOR, 0u64, 100_000_000u64),
            (USER, 1, 100_000_000),
            (WITNESS, 0, 100_000_000),
            (WITNESS2, 0, 100_000_000),
            (DEST, 0, 100_000_000),
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
                    "SignerQuorum": quorum,
                    "SignerEntries": [
                        {"SignerEntry": {"Account": WITNESS, "SignerWeight": 1}},
                        {"SignerEntry": {"Account": WITNESS2, "SignerWeight": 1}},
                    ],
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        ledger
            .put_state(
                claim_key,
                serde_json::to_vec(&serde_json::json!({
                    "LedgerEntryType": "XChainOwnedClaimID",
                    "Account": USER,
                    "XChainBridge": bridge_spec(),
                    "XChainClaimID": "1",
                    "OtherChainSource": OCS,
                    "SignatureReward": "100",
                    "Flags": 0,
                }))
                .unwrap(),
            )
            .unwrap();

        ledger
    }

    fn attestation_tx(signer: &str) -> serde_json::Value {
        serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": signer,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "OtherChainSource": OCS,
            "Destination": DEST,
            "Amount": "10000000",
            "AttestationSignerAccount": signer,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": signer,
            "WasLockingChainSend": 1,
            "Fee": "12",
        })
    }

    #[test]
    fn preflight_missing_attestation_signer() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "10000000",
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
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
            XChainAddClaimAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_claim_id() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "Destination": DEST,
            "Amount": "10000000",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
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
            XChainAddClaimAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_no_claim_id_entry() {
        let ledger = setup(1);
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let mut tx = attestation_tx(WITNESS);
        tx["XChainClaimID"] = serde_json::Value::String("999".into());
        let ctx = PreclaimContext {
            tx: &tx,
            view: &view,
            rules: &rules,
        };
        assert_eq!(
            XChainAddClaimAttestationTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainNoClaimId)
        );
    }

    #[test]
    fn apply_below_quorum_persists_attestation() {
        let ledger = setup(2);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = attestation_tx(WITNESS);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddClaimAttestationTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        // Quorum (2) not reached: claim id survives with the attestation stored.
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim: serde_json::Value =
            serde_json::from_slice(&sandbox.read(&claim_key).unwrap()).unwrap();
        let atts = claim["XChainClaimAttestations"].as_array().unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(
            atts[0]["XChainClaimProofSig"]["AttestationSignerAccount"]
                .as_str()
                .unwrap(),
            WITNESS
        );
    }

    #[test]
    fn apply_unknown_signer_rejected() {
        let ledger = setup(1);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = attestation_tx(DEST); // DEST is not a witness
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddClaimAttestationTransactor.apply(&mut ctx),
            Err(TransactionResult::TecXChainProofUnknownKey)
        );
    }

    #[test]
    fn apply_quorum_pays_out_and_deletes_claim() {
        let ledger = setup(1);
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = attestation_tx(WITNESS);
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        assert_eq!(
            XChainAddClaimAttestationTransactor.apply(&mut ctx).unwrap(),
            TransactionResult::TesSuccess
        );

        let read = |addr: &str| -> serde_json::Value {
            let id = decode_account_id(addr).unwrap();
            serde_json::from_slice(&sandbox.read(&keylet::account(&id)).unwrap()).unwrap()
        };
        // Door pays the full amount; dest receives it; reward (100) comes from the
        // claim owner and goes to the witness.
        assert_eq!(read(DOOR)["Balance"].as_str().unwrap(), "90000000");
        assert_eq!(read(DEST)["Balance"].as_str().unwrap(), "110000000");
        assert_eq!(read(WITNESS)["Balance"].as_str().unwrap(), "100000100");
        assert_eq!(read(USER)["Balance"].as_str().unwrap(), "99999900");
        // Claim owner loses the owned object.
        assert_eq!(read(USER)["OwnerCount"].as_u64().unwrap(), 0);

        // Claim id deleted.
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        assert!(sandbox.read(&claim_key).is_none());
    }
}
