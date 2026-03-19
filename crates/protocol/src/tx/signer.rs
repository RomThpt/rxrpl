use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_codec::binary;
use rxrpl_crypto::KeyPair;
use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_crypto::key_type::KeyType;
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

use crate::error::ProtocolError;
use crate::tx::common::{Signer, SignerInner};

/// Sign a transaction: serialize for signing, sign with keypair, insert signature fields.
///
/// Returns a new JSON Value with `SigningPubKey` and `TxnSignature` populated.
pub fn sign(tx_json: &Value, key_pair: &KeyPair) -> Result<Value, ProtocolError> {
    let mut json = tx_json.clone();

    // Set SigningPubKey to the hex-encoded public key
    let pub_key_hex = hex::encode_upper(key_pair.public_key.as_bytes());
    json.as_object_mut()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?
        .insert("SigningPubKey".to_string(), Value::String(pub_key_hex));

    // Remove any existing TxnSignature
    if let Some(obj) = json.as_object_mut() {
        obj.remove("TxnSignature");
    }

    // Encode for signing (STX prefix + serialized fields, skipping non-signing fields)
    let signing_bytes = binary::encode_for_signing(&json)?;

    // Sign the encoded bytes
    let signature = match key_pair.key_type {
        KeyType::Secp256k1 => rxrpl_crypto::secp256k1::sign(&signing_bytes, &key_pair.private_key)?,
        KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(&signing_bytes, &key_pair.private_key)?,
    };

    // Insert signature
    let sig_hex = hex::encode_upper(signature.as_bytes());
    json.as_object_mut()
        .unwrap()
        .insert("TxnSignature".to_string(), Value::String(sig_hex));

    Ok(json)
}

/// Encode a signed transaction JSON to a hex blob suitable for `submit`.
pub fn serialize_signed(signed_json: &Value) -> Result<String, ProtocolError> {
    let bytes = binary::encode(signed_json)?;
    Ok(hex::encode_upper(bytes))
}

/// Compute the transaction hash (ID) from a signed transaction JSON.
///
/// This is: SHA-512/2(TXN prefix || serialized_tx)
pub fn compute_tx_hash(signed_json: &Value) -> Result<Hash256, ProtocolError> {
    let tx_bytes = binary::encode(signed_json)?;
    let prefix = HashPrefix::TRANSACTION_ID.to_bytes();
    Ok(sha512_half(&[&prefix, &tx_bytes]))
}

/// Produce a single `Signer` entry for multi-signing a transaction.
///
/// The `signer_account` is the classic address of the account whose key is signing.
pub fn sign_for(
    tx_json: &Value,
    key_pair: &KeyPair,
    signer_account: &str,
) -> Result<Signer, ProtocolError> {
    let mut json = tx_json.clone();
    let obj = json
        .as_object_mut()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?;
    obj.remove("TxnSignature");
    obj.remove("Signers");

    let account_id = decode_account_id(signer_account)?;

    let signing_bytes = binary::encode_for_multisigning(&json, account_id.as_bytes())?;

    let signature = match key_pair.key_type {
        KeyType::Secp256k1 => rxrpl_crypto::secp256k1::sign(&signing_bytes, &key_pair.private_key)?,
        KeyType::Ed25519 => rxrpl_crypto::ed25519::sign(&signing_bytes, &key_pair.private_key)?,
    };

    Ok(Signer {
        signer: SignerInner {
            account: signer_account.to_string(),
            txn_signature: hex::encode_upper(signature.as_bytes()),
            signing_pub_key: hex::encode_upper(key_pair.public_key.as_bytes()),
        },
    })
}

/// Assemble multiple `Signer` entries into a complete multisig transaction.
///
/// Signers are sorted by account ID bytes as required by the XRPL protocol.
pub fn combine_multisig(tx_json: &Value, mut signers: Vec<Signer>) -> Result<Value, ProtocolError> {
    if signers.is_empty() {
        return Err(ProtocolError::InvalidFieldValue(
            "signers list must not be empty".into(),
        ));
    }

    // Sort signers by decoded account_id bytes
    signers.sort_by(|a, b| {
        let id_a = decode_account_id(&a.signer.account)
            .map(|id| *id.as_bytes())
            .unwrap_or([0u8; 20]);
        let id_b = decode_account_id(&b.signer.account)
            .map(|id| *id.as_bytes())
            .unwrap_or([0u8; 20]);
        id_a.cmp(&id_b)
    });

    let mut json = tx_json.clone();
    let obj = json
        .as_object_mut()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?;

    obj.insert("SigningPubKey".to_string(), Value::String(String::new()));
    obj.remove("TxnSignature");

    let signers_json: Vec<Value> = signers
        .iter()
        .map(|s| {
            serde_json::json!({
                "Signer": {
                    "Account": s.signer.account,
                    "TxnSignature": s.signer.txn_signature,
                    "SigningPubKey": s.signer.signing_pub_key,
                }
            })
        })
        .collect();

    obj.insert("Signers".to_string(), Value::Array(signers_json));

    Ok(json)
}

/// Verify all signatures in a multisig transaction's `Signers` array.
///
/// For each signer, reconstructs the multi-signing bytes with that signer's
/// account ID suffix and verifies the signature against the provided public key.
pub fn verify_multisig(signed_tx: &Value) -> Result<(), ProtocolError> {
    let obj = signed_tx
        .as_object()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?;

    let signers_arr = obj
        .get("Signers")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProtocolError::MissingField("Signers".into()))?;

    if signers_arr.is_empty() {
        return Err(ProtocolError::InvalidFieldValue(
            "Signers array is empty".into(),
        ));
    }

    // Prepare the base transaction (remove Signers and TxnSignature)
    let mut base_tx = signed_tx.clone();
    let base_obj = base_tx.as_object_mut().unwrap();
    base_obj.remove("Signers");
    base_obj.remove("TxnSignature");

    for signer_wrapper in signers_arr {
        let signer = signer_wrapper
            .get("Signer")
            .ok_or_else(|| ProtocolError::MissingField("Signer wrapper".into()))?;

        let account = signer
            .get("Account")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProtocolError::MissingField("Signer Account".into()))?;

        let sig_hex = signer
            .get("TxnSignature")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProtocolError::MissingField("Signer TxnSignature".into()))?;

        let pub_key_hex = signer
            .get("SigningPubKey")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProtocolError::MissingField("Signer SigningPubKey".into()))?;

        let account_id = decode_account_id(account)?;
        let pub_key_bytes = hex::decode(pub_key_hex)
            .map_err(|e| ProtocolError::InvalidFieldValue(format!("SigningPubKey: {e}")))?;
        let sig_bytes = hex::decode(sig_hex)
            .map_err(|e| ProtocolError::InvalidFieldValue(format!("TxnSignature: {e}")))?;

        let key_type = KeyType::from_public_key(&pub_key_bytes).ok_or_else(|| {
            ProtocolError::InvalidFieldValue("unrecognized public key prefix".into())
        })?;

        let signing_bytes = binary::encode_for_multisigning(&base_tx, account_id.as_bytes())?;

        let valid = match key_type {
            KeyType::Ed25519 => {
                rxrpl_crypto::ed25519::verify(&signing_bytes, &pub_key_bytes, &sig_bytes)
            }
            KeyType::Secp256k1 => {
                rxrpl_crypto::secp256k1::verify(&signing_bytes, &pub_key_bytes, &sig_bytes)
            }
        };

        if !valid {
            return Err(ProtocolError::Signing(format!(
                "multisig verification failed for account {account}"
            )));
        }
    }

    Ok(())
}

/// Verify the signature on a signed transaction JSON.
///
/// Automatically detects single-sig vs multisig: if a non-empty `Signers`
/// array is present, delegates to `verify_multisig`. Otherwise verifies
/// the single `SigningPubKey`/`TxnSignature` pair.
///
/// Returns `Ok(())` if valid, or an error describing why verification failed.
pub fn verify_signature(signed_tx: &Value) -> Result<(), ProtocolError> {
    let obj = signed_tx
        .as_object()
        .ok_or_else(|| ProtocolError::Serialization("expected JSON object".into()))?;

    // Delegate to multisig verification if Signers array is present and non-empty
    if let Some(signers) = obj.get("Signers") {
        if let Some(arr) = signers.as_array() {
            if !arr.is_empty() {
                return verify_multisig(signed_tx);
            }
        }
    }

    let pub_key_hex = obj
        .get("SigningPubKey")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::MissingField("SigningPubKey".into()))?;

    if pub_key_hex.is_empty() {
        return Err(ProtocolError::MissingField("SigningPubKey is empty".into()));
    }

    let sig_hex = obj
        .get("TxnSignature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::MissingField("TxnSignature".into()))?;

    let pub_key_bytes = hex::decode(pub_key_hex)
        .map_err(|e| ProtocolError::InvalidFieldValue(format!("SigningPubKey: {e}")))?;
    let sig_bytes = hex::decode(sig_hex)
        .map_err(|e| ProtocolError::InvalidFieldValue(format!("TxnSignature: {e}")))?;

    let key_type = KeyType::from_public_key(&pub_key_bytes)
        .ok_or_else(|| ProtocolError::InvalidFieldValue("unrecognized public key prefix".into()))?;

    // Remove TxnSignature from a clone, then encode for signing
    let mut for_signing = signed_tx.clone();
    for_signing.as_object_mut().unwrap().remove("TxnSignature");

    let signing_bytes = binary::encode_for_signing(&for_signing)?;

    let valid = match key_type {
        KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::verify(&signing_bytes, &pub_key_bytes, &sig_bytes)
        }
        KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::verify(&signing_bytes, &pub_key_bytes, &sig_bytes)
        }
    };

    if valid {
        Ok(())
    } else {
        Err(ProtocolError::Signing(
            "signature verification failed".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_codec::address::classic::encode_classic_address_from_pubkey;
    use rxrpl_crypto::{KeyType, Seed};

    fn test_keypair(key_type: KeyType) -> KeyPair {
        let seed = Seed::from_passphrase("test_signing");
        KeyPair::from_seed(&seed, key_type)
    }

    fn sample_payment(kp: &KeyPair) -> Value {
        let sender = encode_classic_address_from_pubkey(kp.public_key.as_bytes());
        // Use a second keypair for the destination
        let dest_seed = Seed::from_passphrase("test_destination");
        let dest_kp = KeyPair::from_seed(&dest_seed, KeyType::Ed25519);
        let destination = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());
        serde_json::json!({
            "TransactionType": "Payment",
            "Account": sender,
            "Destination": destination,
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        })
    }

    #[test]
    fn sign_with_secp256k1() {
        let kp = test_keypair(KeyType::Secp256k1);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();

        assert!(signed["SigningPubKey"].is_string());
        assert!(signed["TxnSignature"].is_string());

        let pub_key = signed["SigningPubKey"].as_str().unwrap();
        assert_eq!(pub_key, hex::encode_upper(kp.public_key.as_bytes()));

        let sig_hex = signed["TxnSignature"].as_str().unwrap();
        assert!(!sig_hex.is_empty());
    }

    #[test]
    fn sign_with_ed25519() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();

        assert!(signed["SigningPubKey"].is_string());
        assert!(signed["TxnSignature"].is_string());

        let sig_hex = signed["TxnSignature"].as_str().unwrap();
        // Ed25519 signatures are exactly 64 bytes = 128 hex chars
        assert_eq!(sig_hex.len(), 128);
    }

    #[test]
    fn serialize_signed_produces_hex() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();
        let blob = serialize_signed(&signed).unwrap();
        assert!(!blob.is_empty());
        // Must be valid hex
        assert!(hex::decode(&blob).is_ok());
    }

    #[test]
    fn compute_tx_hash_deterministic() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();

        let hash1 = compute_tx_hash(&signed).unwrap();
        let hash2 = compute_tx_hash(&signed).unwrap();
        assert_eq!(hash1, hash2);
        assert!(!hash1.is_zero());
    }

    #[test]
    fn sign_does_not_mutate_original() {
        let kp = test_keypair(KeyType::Secp256k1);
        let tx = sample_payment(&kp);
        let _signed = sign(&tx, &kp).unwrap();
        // Original should not have signature fields
        assert!(tx.get("TxnSignature").is_none());
    }

    #[test]
    fn verify_ed25519_roundtrip() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();
        verify_signature(&signed).unwrap();
    }

    #[test]
    fn verify_secp256k1_roundtrip() {
        let kp = test_keypair(KeyType::Secp256k1);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();
        verify_signature(&signed).unwrap();
    }

    #[test]
    fn verify_tampered_signature() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let mut signed = sign(&tx, &kp).unwrap();

        // Flip the last byte of the signature
        let sig = signed["TxnSignature"].as_str().unwrap().to_string();
        let mut sig_bytes = hex::decode(&sig).unwrap();
        let last = sig_bytes.last_mut().unwrap();
        *last ^= 0xFF;
        signed["TxnSignature"] = Value::String(hex::encode_upper(&sig_bytes));

        assert!(verify_signature(&signed).is_err());
    }

    #[test]
    fn verify_wrong_pubkey() {
        let kp_a = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp_a);
        let mut signed = sign(&tx, &kp_a).unwrap();

        // Replace SigningPubKey with a different key
        let kp_b = KeyPair::from_seed(&Seed::from_passphrase("different_key"), KeyType::Ed25519);
        signed["SigningPubKey"] = Value::String(hex::encode_upper(kp_b.public_key.as_bytes()));

        assert!(verify_signature(&signed).is_err());
    }

    #[test]
    fn verify_missing_fields() {
        let kp = test_keypair(KeyType::Ed25519);
        let tx = sample_payment(&kp);
        let signed = sign(&tx, &kp).unwrap();

        // Missing TxnSignature
        let mut no_sig = signed.clone();
        no_sig.as_object_mut().unwrap().remove("TxnSignature");
        assert!(verify_signature(&no_sig).is_err());

        // Missing SigningPubKey
        let mut no_pub = signed.clone();
        no_pub.as_object_mut().unwrap().remove("SigningPubKey");
        assert!(verify_signature(&no_pub).is_err());
    }

    // -- Multisig tests --

    fn multisig_keypair(passphrase: &str, key_type: KeyType) -> KeyPair {
        let seed = Seed::from_passphrase(passphrase);
        KeyPair::from_seed(&seed, key_type)
    }

    fn sample_multisig_tx() -> Value {
        let kp = multisig_keypair("multisig_account", KeyType::Ed25519);
        let sender = encode_classic_address_from_pubkey(kp.public_key.as_bytes());
        let dest_kp = multisig_keypair("multisig_dest", KeyType::Ed25519);
        let dest = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());
        serde_json::json!({
            "TransactionType": "Payment",
            "Account": sender,
            "Destination": dest,
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        })
    }

    #[test]
    fn sign_for_ed25519_produces_signer() {
        let kp = multisig_keypair("signer_ed", KeyType::Ed25519);
        let account = encode_classic_address_from_pubkey(kp.public_key.as_bytes());
        let tx = sample_multisig_tx();
        let signer = sign_for(&tx, &kp, &account).unwrap();

        assert_eq!(signer.signer.account, account);
        assert!(!signer.signer.txn_signature.is_empty());
        assert!(!signer.signer.signing_pub_key.is_empty());
    }

    #[test]
    fn sign_for_secp256k1_produces_signer() {
        let kp = multisig_keypair("signer_secp", KeyType::Secp256k1);
        let account = encode_classic_address_from_pubkey(kp.public_key.as_bytes());
        let tx = sample_multisig_tx();
        let signer = sign_for(&tx, &kp, &account).unwrap();

        assert_eq!(signer.signer.account, account);
        assert!(!signer.signer.txn_signature.is_empty());
        assert!(!signer.signer.signing_pub_key.is_empty());
    }

    #[test]
    fn combine_multisig_sets_empty_signing_pub_key() {
        let kp = multisig_keypair("signer_combine", KeyType::Ed25519);
        let account = encode_classic_address_from_pubkey(kp.public_key.as_bytes());
        let tx = sample_multisig_tx();
        let signer = sign_for(&tx, &kp, &account).unwrap();
        let combined = combine_multisig(&tx, vec![signer]).unwrap();

        assert_eq!(combined["SigningPubKey"].as_str().unwrap(), "");
        assert!(combined.get("TxnSignature").is_none());
        assert!(combined["Signers"].is_array());
    }

    #[test]
    fn combine_multisig_sorts_signers() {
        let kp1 = multisig_keypair("sort_a", KeyType::Ed25519);
        let kp2 = multisig_keypair("sort_b", KeyType::Ed25519);
        let kp3 = multisig_keypair("sort_c", KeyType::Ed25519);

        let acc1 = encode_classic_address_from_pubkey(kp1.public_key.as_bytes());
        let acc2 = encode_classic_address_from_pubkey(kp2.public_key.as_bytes());
        let acc3 = encode_classic_address_from_pubkey(kp3.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let s1 = sign_for(&tx, &kp1, &acc1).unwrap();
        let s2 = sign_for(&tx, &kp2, &acc2).unwrap();
        let s3 = sign_for(&tx, &kp3, &acc3).unwrap();

        // Pass in reverse order to test sorting
        let combined = combine_multisig(&tx, vec![s3, s1, s2]).unwrap();
        let signers = combined["Signers"].as_array().unwrap();

        // Verify they are sorted by account_id bytes
        let mut prev_id = [0u8; 20];
        for signer in signers {
            let account = signer["Signer"]["Account"].as_str().unwrap();
            let id = decode_account_id(account).unwrap();
            assert!(*id.as_bytes() >= prev_id);
            prev_id = *id.as_bytes();
        }
    }

    #[test]
    fn combine_multisig_rejects_empty() {
        let tx = sample_multisig_tx();
        assert!(combine_multisig(&tx, vec![]).is_err());
    }

    #[test]
    fn verify_multisig_ed25519_roundtrip() {
        let kp1 = multisig_keypair("verify_ed_1", KeyType::Ed25519);
        let kp2 = multisig_keypair("verify_ed_2", KeyType::Ed25519);
        let acc1 = encode_classic_address_from_pubkey(kp1.public_key.as_bytes());
        let acc2 = encode_classic_address_from_pubkey(kp2.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let s1 = sign_for(&tx, &kp1, &acc1).unwrap();
        let s2 = sign_for(&tx, &kp2, &acc2).unwrap();

        let combined = combine_multisig(&tx, vec![s1, s2]).unwrap();
        verify_multisig(&combined).unwrap();
    }

    #[test]
    fn verify_multisig_secp256k1_roundtrip() {
        let kp1 = multisig_keypair("verify_secp_1", KeyType::Secp256k1);
        let kp2 = multisig_keypair("verify_secp_2", KeyType::Secp256k1);
        let acc1 = encode_classic_address_from_pubkey(kp1.public_key.as_bytes());
        let acc2 = encode_classic_address_from_pubkey(kp2.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let s1 = sign_for(&tx, &kp1, &acc1).unwrap();
        let s2 = sign_for(&tx, &kp2, &acc2).unwrap();

        let combined = combine_multisig(&tx, vec![s1, s2]).unwrap();
        verify_multisig(&combined).unwrap();
    }

    #[test]
    fn verify_multisig_mixed_key_types() {
        let kp_ed = multisig_keypair("mixed_ed", KeyType::Ed25519);
        let kp_secp = multisig_keypair("mixed_secp", KeyType::Secp256k1);
        let acc_ed = encode_classic_address_from_pubkey(kp_ed.public_key.as_bytes());
        let acc_secp = encode_classic_address_from_pubkey(kp_secp.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let s_ed = sign_for(&tx, &kp_ed, &acc_ed).unwrap();
        let s_secp = sign_for(&tx, &kp_secp, &acc_secp).unwrap();

        let combined = combine_multisig(&tx, vec![s_ed, s_secp]).unwrap();
        verify_multisig(&combined).unwrap();
    }

    #[test]
    fn verify_multisig_tampered_signature() {
        let kp = multisig_keypair("tamper_ms", KeyType::Ed25519);
        let acc = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let signer = sign_for(&tx, &kp, &acc).unwrap();
        let mut combined = combine_multisig(&tx, vec![signer]).unwrap();

        // Flip a byte in the signature
        let sig = combined["Signers"][0]["Signer"]["TxnSignature"]
            .as_str()
            .unwrap()
            .to_string();
        let mut sig_bytes = hex::decode(&sig).unwrap();
        *sig_bytes.last_mut().unwrap() ^= 0xFF;
        combined["Signers"][0]["Signer"]["TxnSignature"] =
            Value::String(hex::encode_upper(&sig_bytes));

        assert!(verify_multisig(&combined).is_err());
    }

    #[test]
    fn verify_multisig_wrong_pubkey() {
        let kp1 = multisig_keypair("wrong_pk_1", KeyType::Ed25519);
        let kp2 = multisig_keypair("wrong_pk_2", KeyType::Ed25519);
        let acc1 = encode_classic_address_from_pubkey(kp1.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let signer = sign_for(&tx, &kp1, &acc1).unwrap();
        let mut combined = combine_multisig(&tx, vec![signer]).unwrap();

        // Swap the public key with a different one
        combined["Signers"][0]["Signer"]["SigningPubKey"] =
            Value::String(hex::encode_upper(kp2.public_key.as_bytes()));

        assert!(verify_multisig(&combined).is_err());
    }

    #[test]
    fn verify_multisig_empty_signers() {
        let tx = sample_multisig_tx();
        let mut multisig_tx = tx.clone();
        multisig_tx["SigningPubKey"] = Value::String(String::new());
        multisig_tx["Signers"] = serde_json::json!([]);

        assert!(verify_multisig(&multisig_tx).is_err());
    }

    #[test]
    fn verify_multisig_missing_signers() {
        let tx = sample_multisig_tx();
        let mut multisig_tx = tx.clone();
        multisig_tx["SigningPubKey"] = Value::String(String::new());

        assert!(verify_multisig(&multisig_tx).is_err());
    }

    #[test]
    fn verify_signature_delegates_to_multisig() {
        let kp1 = multisig_keypair("delegate_1", KeyType::Ed25519);
        let kp2 = multisig_keypair("delegate_2", KeyType::Secp256k1);
        let acc1 = encode_classic_address_from_pubkey(kp1.public_key.as_bytes());
        let acc2 = encode_classic_address_from_pubkey(kp2.public_key.as_bytes());

        let tx = sample_multisig_tx();
        let s1 = sign_for(&tx, &kp1, &acc1).unwrap();
        let s2 = sign_for(&tx, &kp2, &acc2).unwrap();

        let combined = combine_multisig(&tx, vec![s1, s2]).unwrap();
        // Call verify_signature (not verify_multisig directly) -- should delegate
        verify_signature(&combined).unwrap();
    }
}
