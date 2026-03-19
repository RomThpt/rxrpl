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

        Ok(())
    }

    fn apply(&self, ctx: &mut ApplyContext<'_>) -> Result<TransactionResult, TransactionResult> {
        let account_str = helpers::get_account(ctx.tx)?;
        let account_id =
            decode_account_id(account_str).map_err(|_| TransactionResult::TemInvalidAccountId)?;

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

        // Add attestation to claim ID entry's Attestations array
        let claim_key = keylet::xchain_claim_id(&bridge_data, claim_id);
        let claim_bytes = ctx
            .view
            .read(&claim_key)
            .ok_or(TransactionResult::TecXChainNoClaimId)?;
        let mut claim_entry: serde_json::Value =
            serde_json::from_slice(&claim_bytes).map_err(|_| TransactionResult::TefInternal)?;

        let attestation = serde_json::json!({
            "AttestationSignerAccount": attestation_signer,
            "PublicKey": public_key,
            "Amount": amount.to_string(),
            "AttestationRewardAccount": reward_account,
            "WasLockingChainSend": was_locking,
        });

        let attestations = claim_entry
            .get_mut("Attestations")
            .and_then(|v| v.as_array_mut())
            .ok_or(TransactionResult::TefInternal)?;
        attestations.push(attestation);

        let claim_data =
            serde_json::to_vec(&claim_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(claim_key, claim_data)
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
    const WITNESS: &str = "r3kmLJN5D28dHuH8vZNUZpMC43pEHpaocV";
    const DEST: &str = "rGWrZyQqhTp9Xu7G5iFQmGEFRWKvRm3TGr";

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
        for addr in [DOOR, USER, WITNESS] {
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

        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedClaimID",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "OtherChainSource": DOOR,
            "Attestations": [],
            "Flags": 0,
        });
        ledger
            .put_state(claim_key, serde_json::to_vec(&claim_entry).unwrap())
            .unwrap();

        ledger
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
        let ledger = setup_with_bridge_and_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "999",
            "Destination": DEST,
            "Amount": "10000000",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
            "Fee": "12",
        });
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
    fn apply_adds_attestation() {
        let ledger = setup_with_bridge_and_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "10000000",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
            "Fee": "12",
        });

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };

        let result = XChainAddClaimAttestationTransactor.apply(&mut ctx).unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify attestation added
        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_bytes = sandbox.read(&claim_key).unwrap();
        let claim: serde_json::Value = serde_json::from_slice(&claim_bytes).unwrap();
        let attestations = claim["Attestations"].as_array().unwrap();
        assert_eq!(attestations.len(), 1);
        assert_eq!(
            attestations[0]["AttestationSignerAccount"]
                .as_str()
                .unwrap(),
            WITNESS
        );
        assert_eq!(attestations[0]["Amount"].as_str().unwrap(), "10000000");
    }

    #[test]
    fn apply_adds_multiple_attestations() {
        let ledger = setup_with_bridge_and_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddClaimAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainClaimID": "1",
            "Destination": DEST,
            "Amount": "10000000",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
            "Signature": "DEADBEEF",
            "AttestationRewardAccount": WITNESS,
            "WasLockingChainSend": 1,
            "Fee": "12",
        });

        // First attestation
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        XChainAddClaimAttestationTransactor.apply(&mut ctx).unwrap();

        // Second attestation
        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        XChainAddClaimAttestationTransactor.apply(&mut ctx).unwrap();

        let bridge_data = bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let claim_key = keylet::xchain_claim_id(&bridge_data, 1);
        let claim_bytes = sandbox.read(&claim_key).unwrap();
        let claim: serde_json::Value = serde_json::from_slice(&claim_bytes).unwrap();
        let attestations = claim["Attestations"].as_array().unwrap();
        assert_eq!(attestations.len(), 2);
    }
}
