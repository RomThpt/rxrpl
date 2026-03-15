use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_protocol::{keylet, TransactionResult};

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
        helpers::get_str_field(ctx.tx, "PublicKey")
            .ok_or(TransactionResult::TemMalformed)?;
        helpers::get_str_field(ctx.tx, "Signature")
            .ok_or(TransactionResult::TemMalformed)?;
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
        let door_id = decode_account_id(locking_door)
            .map_err(|_| TransactionResult::TemInvalidAccountId)?;
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        if !ctx.view.exists(&bridge_key) {
            return Err(TransactionResult::TecNoEntry);
        }

        // Create account claim ID entry must exist
        let count =
            helpers::get_u64_str_field(ctx.tx, "XChainAccountCreateCount").unwrap();
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, count);
        if !ctx.view.exists(&create_key) {
            return Err(TransactionResult::TecXChainAccountCreatePastSeq);
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
        let count =
            helpers::get_u64_str_field(ctx.tx, "XChainAccountCreateCount").unwrap();

        let attestation_signer = helpers::get_str_field(ctx.tx, "AttestationSignerAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let public_key = helpers::get_str_field(ctx.tx, "PublicKey")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let amount =
            helpers::get_xrp_amount(ctx.tx).ok_or(TransactionResult::TemBadAmount)?;
        let sig_reward =
            helpers::get_u64_str_field(ctx.tx, "SignatureReward").unwrap_or(0);
        let reward_account = helpers::get_str_field(ctx.tx, "AttestationRewardAccount")
            .ok_or(TransactionResult::TemMalformed)?
            .to_string();
        let was_locking = helpers::get_u32_field(ctx.tx, "WasLockingChainSend")
            .ok_or(TransactionResult::TemMalformed)?;

        // Add attestation to entry's Attestations array
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, count);
        let create_bytes = ctx
            .view
            .read(&create_key)
            .ok_or(TransactionResult::TecXChainAccountCreatePastSeq)?;
        let mut create_entry: serde_json::Value = serde_json::from_slice(&create_bytes)
            .map_err(|_| TransactionResult::TefInternal)?;

        let attestation = serde_json::json!({
            "AttestationSignerAccount": attestation_signer,
            "PublicKey": public_key,
            "Amount": amount.to_string(),
            "SignatureReward": sig_reward.to_string(),
            "AttestationRewardAccount": reward_account,
            "WasLockingChainSend": was_locking,
        });

        let attestations = create_entry
            .get_mut("Attestations")
            .and_then(|v| v.as_array_mut())
            .ok_or(TransactionResult::TefInternal)?;
        attestations.push(attestation);

        let create_data =
            serde_json::to_vec(&create_entry).map_err(|_| TransactionResult::TefInternal)?;
        ctx.view
            .update(create_key, create_data)
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
    const NEW_ACCOUNT: &str = "rGWrZyQqhTp9Xu7G5iFQmGEFRWKvRm3TGr";

    fn bridge_spec() -> serde_json::Value {
        serde_json::json!({
            "LockingChainDoor": DOOR,
            "LockingChainIssue": "XRP",
            "IssuingChainDoor": USER,
            "IssuingChainIssue": "XRP"
        })
    }

    fn setup_with_bridge_and_create_claim() -> Ledger {
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
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let bridge_key = keylet::bridge(&door_id, &bridge_data);
        let bridge_entry = serde_json::json!({
            "LedgerEntryType": "Bridge",
            "Account": DOOR,
            "XChainBridge": bridge_spec(),
            "SignatureReward": "100",
            "XChainClaimID": "0",
            "XChainAccountCreateCount": "1",
            "Flags": 0,
        });
        ledger
            .put_state(bridge_key, serde_json::to_vec(&bridge_entry).unwrap())
            .unwrap();

        // Create account claim entry
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, 1);
        let create_entry = serde_json::json!({
            "LedgerEntryType": "XChainOwnedCreateAccountClaimID",
            "Account": USER,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": "1",
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
            "Attestations": [],
            "Flags": 0,
        });
        ledger
            .put_state(create_key, serde_json::to_vec(&create_entry).unwrap())
            .unwrap();

        ledger
    }

    #[test]
    fn preflight_missing_count() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
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
            XChainAddAccountCreateAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preflight_missing_signature() {
        let tx = serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": "1",
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
            "AttestationSignerAccount": WITNESS,
            "PublicKey": "0388935426E0D08083314842EDFCBEE2EA9B6B197B0D9A0BA4AA3B1D7381AFBFEA",
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
            XChainAddAccountCreateAttestationTransactor.preflight(&ctx),
            Err(TransactionResult::TemMalformed)
        );
    }

    #[test]
    fn preclaim_no_create_claim_entry() {
        let ledger = setup_with_bridge_and_create_claim();
        let view = LedgerView::with_fees(&ledger, FeeSettings::default());
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": "999",
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
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
            XChainAddAccountCreateAttestationTransactor.preclaim(&ctx),
            Err(TransactionResult::TecXChainAccountCreatePastSeq)
        );
    }

    #[test]
    fn apply_adds_attestation() {
        let ledger = setup_with_bridge_and_create_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": "1",
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
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

        let result = XChainAddAccountCreateAttestationTransactor
            .apply(&mut ctx)
            .unwrap();
        assert_eq!(result, TransactionResult::TesSuccess);

        // Verify attestation added
        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, 1);
        let create_bytes = sandbox.read(&create_key).unwrap();
        let create: serde_json::Value =
            serde_json::from_slice(&create_bytes).unwrap();
        let attestations = create["Attestations"].as_array().unwrap();
        assert_eq!(attestations.len(), 1);
        assert_eq!(
            attestations[0]["AttestationSignerAccount"].as_str().unwrap(),
            WITNESS
        );
    }

    #[test]
    fn apply_adds_multiple_attestations() {
        let ledger = setup_with_bridge_and_create_claim();
        let fees = FeeSettings::default();
        let view = LedgerView::with_fees(&ledger, fees.clone());
        let mut sandbox = Sandbox::new(&view);
        let rules = Rules::new();

        let tx = serde_json::json!({
            "TransactionType": "XChainAddAccountCreateAttestation",
            "Account": WITNESS,
            "XChainBridge": bridge_spec(),
            "XChainAccountCreateCount": "1",
            "Destination": NEW_ACCOUNT,
            "Amount": "10000000",
            "SignatureReward": "100",
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
        XChainAddAccountCreateAttestationTransactor
            .apply(&mut ctx)
            .unwrap();

        let mut ctx = ApplyContext {
            tx: &tx,
            view: &mut sandbox,
            rules: &rules,
            fees: &fees,
        };
        XChainAddAccountCreateAttestationTransactor
            .apply(&mut ctx)
            .unwrap();

        let bridge_data =
            bridge_helpers::serialize_bridge_spec(&bridge_spec()).unwrap();
        let create_key =
            keylet::xchain_create_account_claim_id(&bridge_data, 1);
        let create_bytes = sandbox.read(&create_key).unwrap();
        let create: serde_json::Value =
            serde_json::from_slice(&create_bytes).unwrap();
        let attestations = create["Attestations"].as_array().unwrap();
        assert_eq!(attestations.len(), 2);
    }
}
