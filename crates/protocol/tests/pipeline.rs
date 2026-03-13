use rxrpl_codec::address::{decode_seed, encode_classic_address_from_pubkey, encode_seed};
use rxrpl_crypto::{KeyPair, KeyType, Seed};
use rxrpl_protocol::tx::common::Transaction;
use rxrpl_protocol::tx::{
    combine_multisig, compute_tx_hash, serialize_signed, sign, verify_multisig, verify_signature,
    Payment,
};
use rxrpl_protocol::Wallet;

/// End-to-end offline pipeline: build -> sign -> serialize -> compute hash.
#[test]
fn offline_payment_pipeline_ed25519() {
    let seed = Seed::from_passphrase("integration_test_seed");
    let kp = KeyPair::from_seed(&seed, KeyType::Ed25519);
    let address = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

    let dest_seed = Seed::from_passphrase("integration_test_dest");
    let dest_kp = KeyPair::from_seed(&dest_seed, KeyType::Ed25519);
    let dest = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    // Build
    let mut payment = Payment::xrp(&address, &dest, 1_000_000, "12");
    payment.common.sequence = Some(1);

    let tx_json = payment.to_json().unwrap();
    assert_eq!(tx_json["TransactionType"], "Payment");
    assert_eq!(tx_json["Amount"], "1000000");

    // Sign
    let signed = sign(&tx_json, &kp).unwrap();
    assert!(signed["TxnSignature"].is_string());
    assert!(signed["SigningPubKey"].is_string());

    // Serialize
    let blob = serialize_signed(&signed).unwrap();
    assert!(!blob.is_empty());
    assert!(hex::decode(&blob).is_ok());

    // Compute hash
    let hash = compute_tx_hash(&signed).unwrap();
    assert!(!hash.is_zero());

    // Hash is deterministic
    let hash2 = compute_tx_hash(&signed).unwrap();
    assert_eq!(hash, hash2);
}

#[test]
fn offline_payment_pipeline_secp256k1() {
    let seed = Seed::from_passphrase("integration_test_secp");
    let kp = KeyPair::from_seed(&seed, KeyType::Secp256k1);
    let address = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

    let dest_seed = Seed::from_passphrase("integration_test_dest2");
    let dest_kp = KeyPair::from_seed(&dest_seed, KeyType::Ed25519);
    let dest = encode_classic_address_from_pubkey(dest_kp.public_key.as_bytes());

    let mut payment = Payment::xrp(&address, &dest, 500_000, "10");
    payment.common.sequence = Some(5);
    payment.common.last_ledger_sequence = Some(100);

    let tx_json = payment.to_json().unwrap();
    let signed = sign(&tx_json, &kp).unwrap();
    let blob = serialize_signed(&signed).unwrap();
    let hash = compute_tx_hash(&signed).unwrap();

    assert!(!blob.is_empty());
    assert!(!hash.is_zero());
}

#[test]
fn seed_roundtrip_in_pipeline() {
    // Encode a seed, decode it, derive keypair, sign a tx
    let seed = Seed::random();
    let encoded = encode_seed(seed.as_bytes(), KeyType::Ed25519).unwrap();
    assert!(encoded.starts_with("sEd"));

    let (entropy, _kt) = decode_seed(&encoded).unwrap();
    assert_eq!(entropy, *seed.as_bytes());

    let recovered = Seed::from_bytes(entropy);
    let kp = KeyPair::from_seed(&recovered, KeyType::Ed25519);
    let address = encode_classic_address_from_pubkey(kp.public_key.as_bytes());

    let mut payment = Payment::xrp(&address, "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe", 100, "12");
    payment.common.sequence = Some(1);

    let tx_json = payment.to_json().unwrap();
    let signed = sign(&tx_json, &kp).unwrap();
    let blob = serialize_signed(&signed).unwrap();

    assert!(!blob.is_empty());
    assert!(hex::decode(&blob).is_ok());
}

#[test]
fn sign_command_simulation() {
    // Simulate what the CLI `sign` command does
    let seed = Seed::from_passphrase("cli_sign_test");
    let encoded = encode_seed(seed.as_bytes(), KeyType::Ed25519).unwrap();

    // Decode the seed (as the CLI would)
    let (entropy, _) = decode_seed(&encoded).unwrap();
    let recovered = Seed::from_bytes(entropy);
    let kp = KeyPair::from_seed(&recovered, KeyType::Ed25519);

    // Inline JSON (as the CLI would receive)
    let tx_json: serde_json::Value = serde_json::json!({
        "TransactionType": "Payment",
        "Account": encode_classic_address_from_pubkey(kp.public_key.as_bytes()),
        "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
        "Amount": "2000000",
        "Fee": "12",
        "Sequence": 10
    });

    let signed = sign(&tx_json, &kp).unwrap();
    let blob = serialize_signed(&signed).unwrap();
    let hash = compute_tx_hash(&signed).unwrap();

    assert!(!blob.is_empty());
    assert!(!hash.is_zero());
    assert_eq!(hash.to_string().len(), 64); // 32 bytes = 64 hex chars
}

#[test]
fn wallet_sign_and_verify_roundtrip() {
    for key_type in [KeyType::Ed25519, KeyType::Secp256k1] {
        let wallet = Wallet::generate(key_type);
        let tx = serde_json::json!({
            "TransactionType": "Payment",
            "Account": wallet.address,
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
            "Amount": "1000000",
            "Fee": "12",
            "Sequence": 1
        });

        let signed = wallet.sign(&tx).unwrap();
        verify_signature(&signed).unwrap();
    }
}

#[test]
fn wallet_sign_and_serialize_pipeline() {
    let wallet = Wallet::generate(KeyType::Ed25519);
    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": wallet.address,
        "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
        "Amount": "500000",
        "Fee": "12",
        "Sequence": 1
    });

    let (blob, hash) = wallet.sign_and_serialize(&tx).unwrap();
    assert!(!blob.is_empty());
    assert!(hex::decode(&blob).is_ok());
    assert!(!hash.is_zero());

    // Verify the signed tx
    let signed = wallet.sign(&tx).unwrap();
    verify_signature(&signed).unwrap();

    // Seed roundtrip through Wallet
    let encoded = wallet.seed_encoded().unwrap();
    let restored = Wallet::from_seed(&encoded).unwrap();
    assert_eq!(wallet.address, restored.address);
}

#[test]
fn multisig_sign_combine_verify_pipeline() {
    let w1 = Wallet::from_entropy([30u8; 16], KeyType::Ed25519);
    let w2 = Wallet::from_entropy([31u8; 16], KeyType::Secp256k1);
    let w3 = Wallet::from_entropy([32u8; 16], KeyType::Ed25519);

    let tx = serde_json::json!({
        "TransactionType": "Payment",
        "Account": w1.address,
        "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe",
        "Amount": "1000000",
        "Fee": "12",
        "Sequence": 1
    });

    // Each wallet multisigns
    let s1 = w1.multisign(&tx).unwrap();
    let s2 = w2.multisign(&tx).unwrap();
    let s3 = w3.multisign(&tx).unwrap();

    // Combine
    let combined = combine_multisig(&tx, vec![s1, s2, s3]).unwrap();

    // Verify via verify_multisig directly
    verify_multisig(&combined).unwrap();

    // Verify via verify_signature (should delegate)
    verify_signature(&combined).unwrap();

    // Should still serialize
    let blob = serialize_signed(&combined).unwrap();
    assert!(!blob.is_empty());
    assert!(hex::decode(&blob).is_ok());

    // Hash should be deterministic
    let hash = compute_tx_hash(&combined).unwrap();
    assert!(!hash.is_zero());
    assert_eq!(hash, compute_tx_hash(&combined).unwrap());
}
