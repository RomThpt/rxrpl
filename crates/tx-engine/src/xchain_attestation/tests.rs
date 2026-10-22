use super::*;
use crate::fees::FeeSettings;
use crate::handlers::xchain_add_account_create_attestation::XChainAddAccountCreateAttestationTransactor;
use crate::handlers::xchain_add_claim_attestation::XChainAddClaimAttestationTransactor;
use crate::transactor::{PreclaimContext, PreflightContext, Transactor};
use crate::view::ledger_view::LedgerView;
use rxrpl_amendment::Rules;
use rxrpl_codec::address::classic::encode_classic_address_from_pubkey;
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_ledger::Ledger;
use serde_json::json;

const DOOR: &str = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
const USER: &str = "rDTXLQ7ZKZVKz33zJbHjgVShjsBnqMBhmN";

fn handler(create: bool) -> Box<dyn Transactor> {
    if create {
        Box::new(XChainAddAccountCreateAttestationTransactor)
    } else {
        Box::new(XChainAddClaimAttestationTransactor)
    }
}

fn signed_tx(key_type: KeyType, create: bool) -> Value {
    let key = KeyPair::from_seed(&Seed::from_bytes([19; 16]), key_type);
    let signer = encode_classic_address_from_pubkey(key.public_key.as_bytes());
    let bridge = json!({
        "LockingChainDoor": DOOR, "LockingChainIssue": {"currency": "XRP"},
        "IssuingChainDoor": USER, "IssuingChainIssue": {"currency": "XRP"},
    });
    // Construct the signed object independently, following XChainAttestations.cpp.
    let mut message = json!({
        "XChainBridge": bridge, "Amount": "20000000", "OtherChainSource": USER,
        "AttestationRewardAccount": signer, "WasLockingChainSend": 0, "Destination": USER,
    });
    let tx_type = if create {
        message["XChainAccountCreateCount"] = json!("1");
        message["SignatureReward"] = json!("100");
        "XChainAddAccountCreateAttestation"
    } else {
        message["XChainClaimID"] = json!("1");
        "XChainAddClaimAttestation"
    };
    let bytes = rxrpl_codec::binary::encode(&message).unwrap();
    let signature = match key_type {
        KeyType::Secp256k1 => rxrpl_crypto::secp256k1::sign(&bytes, &key.private_key).unwrap(),
        KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(&bytes, &key.private_key).unwrap(),
    };
    message["TransactionType"] = json!(tx_type);
    message["Account"] = json!(DOOR);
    message["Sequence"] = json!(1);
    message["Fee"] = json!("12");
    message["AttestationSignerAccount"] = json!(signer);
    message["PublicKey"] = json!(hex::encode_upper(key.public_key.as_bytes()));
    message["Signature"] = json!(hex::encode_upper(signature.as_bytes()));
    message
}

fn ledger_for(tx: &Value, witness_account: Option<Value>) -> Ledger {
    let mut ledger = Ledger::genesis();
    let door = decode_account_id(DOOR).unwrap();
    ledger
        .put_state(
            keylet::account(&door),
            serde_json::to_vec(&json!({
                "LedgerEntryType": "AccountRoot", "Account": DOOR,
                "Balance": "1000000000", "Sequence": 1, "OwnerCount": 0, "Flags": 0,
            }))
            .unwrap(),
        )
        .unwrap();
    let bridge_key = crate::bridge_helpers::bridge_keylet_for_door(
        &door,
        &tx["XChainBridge"]["LockingChainIssue"],
    );
    ledger
        .put_state(
            bridge_key,
            serde_json::to_vec(&json!({
                "LedgerEntryType": "Bridge", "Account": DOOR, "XChainBridge": tx["XChainBridge"],
            }))
            .unwrap(),
        )
        .unwrap();
    ledger.put_state(keylet::signer_list(&door), serde_json::to_vec(&json!({
        "LedgerEntryType": "SignerList", "SignerQuorum": 1,
        "SignerEntries": [{"SignerEntry": {"Account": tx["AttestationSignerAccount"], "SignerWeight": 1}}],
    })).unwrap()).unwrap();
    let bridge_bytes = crate::bridge_helpers::serialize_bridge_spec(&tx["XChainBridge"]).unwrap();
    ledger
        .put_state(
            keylet::xchain_claim_id(&bridge_bytes, 1),
            serde_json::to_vec(&json!({
                "LedgerEntryType": "XChainOwnedClaimID", "Account": DOOR,
            }))
            .unwrap(),
        )
        .unwrap();
    if let Some(account) = witness_account {
        let id = decode_account_id(tx["AttestationSignerAccount"].as_str().unwrap()).unwrap();
        ledger
            .put_state(keylet::account(&id), serde_json::to_vec(&account).unwrap())
            .unwrap();
    }
    ledger
}

fn preflight(tx: &Value, create: bool) -> Result<(), TransactionResult> {
    handler(create).preflight(&PreflightContext {
        tx,
        rules: &Rules::new(),
        fees: &FeeSettings::default(),
    })
}

fn preclaim(tx: &Value, ledger: &Ledger, create: bool) -> Result<(), TransactionResult> {
    handler(create).preclaim(&PreclaimContext {
        tx,
        rules: &Rules::new(),
        view: &LedgerView::with_fees(ledger, FeeSettings::default()),
    })
}

#[test]
fn witness_proofs_accept_both_key_types_and_reject_tampering() {
    for create in [false, true] {
        for key_type in [KeyType::Secp256k1, KeyType::Ed25519] {
            let tx = signed_tx(key_type, create);
            assert_eq!(preflight(&tx, create), Ok(()));
            for (field, value) in [
                ("Amount", json!("20000001")),
                ("OtherChainSource", json!(DOOR)),
                ("AttestationRewardAccount", json!(DOOR)),
                ("Destination", json!(DOOR)),
                ("WasLockingChainSend", json!(1)),
                ("Signature", json!("DEADBEEF")),
                (
                    if create {
                        "XChainAccountCreateCount"
                    } else {
                        "XChainClaimID"
                    },
                    json!("2"),
                ),
            ] {
                let mut altered = tx.clone();
                altered[field] = value;
                assert_eq!(
                    preflight(&altered, create),
                    Err(TransactionResult::TemXChainBadProof),
                    "{field}"
                );
            }
            if create {
                let mut altered = tx.clone();
                altered["SignatureReward"] = json!("101");
                assert_eq!(
                    preflight(&altered, create),
                    Err(TransactionResult::TemXChainBadProof)
                );
            }
        }
    }
}

#[test]
fn witness_keys_require_authorization_and_respect_disabled_master() {
    for create in [false, true] {
        let tx = signed_tx(KeyType::Ed25519, create);
        assert_eq!(preclaim(&tx, &ledger_for(&tx, None), create), Ok(()));
        let disabled = json!({"Flags": 0x0010_0000});
        assert_eq!(
            preclaim(&tx, &ledger_for(&tx, Some(disabled)), create),
            Err(TransactionResult::TecXChainBadPublicKeyAccountPair)
        );

        // A valid signature cannot impersonate a different witness account.
        let mut forged = tx.clone();
        forged["AttestationSignerAccount"] = json!(USER);
        assert_eq!(preflight(&forged, create), Ok(()));
        assert_eq!(
            preclaim(&forged, &ledger_for(&forged, None), create),
            Err(TransactionResult::TecXChainBadPublicKeyAccountPair)
        );
        let regular = json!({"RegularKey": tx["AttestationSignerAccount"], "Flags": 0x0010_0000});
        assert_eq!(
            preclaim(&forged, &ledger_for(&forged, Some(regular)), create),
            Ok(())
        );

        let mut unknown = tx.clone();
        unknown["AttestationSignerAccount"] = json!(USER);
        assert_eq!(
            preclaim(&unknown, &ledger_for(&tx, None), create),
            Err(TransactionResult::TecNoPermission)
        );
    }
}

#[test]
fn wrong_chain_attestations_are_rejected() {
    for create in [false, true] {
        let mut tx = signed_tx(KeyType::Ed25519, create);
        tx["WasLockingChainSend"] = json!(1);
        assert_eq!(
            preclaim(&tx, &ledger_for(&tx, None), create),
            Err(TransactionResult::TecXChainWrongChain)
        );
    }
}
