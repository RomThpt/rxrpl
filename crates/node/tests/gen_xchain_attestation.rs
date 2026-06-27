// Throwaway helper: sign an XChain claim attestation. The witness signs the
// canonical attestation STObject (raw STObject serialization, no prefix).
//   XA_MSG=/tmp/xa_msg.json XA_SEED=<witnessSeed> XA_OUT=/tmp/xa_sig.json \
//   cargo test -p rxrpl-node --test gen_xchain_attestation -- --ignored --nocapture
#[test]
#[ignore]
fn gen_xchain_attestation() {
    let msg_path = std::env::var("XA_MSG").unwrap();
    let seed = std::env::var("XA_SEED").unwrap();
    let out = std::env::var("XA_OUT").unwrap();

    let msg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&msg_path).unwrap()).unwrap();

    let (entropy, key_type) = rxrpl_codec::address::seed::decode_seed(&seed).unwrap();
    let kp = rxrpl_crypto::KeyPair::from_seed(
        &rxrpl_crypto::seed::Seed::from_bytes(entropy),
        key_type,
    );

    // The attestation message is the raw STObject serialization (all fields,
    // canonical order); secp256k1::sign hashes it (sha512-half) internally.
    let message = rxrpl_codec::binary::encode(&msg).unwrap();
    let sig = match key_type {
        rxrpl_crypto::key_type::KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::sign(&message, &kp.private_key).unwrap()
        }
        rxrpl_crypto::key_type::KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::sign(&message, &kp.private_key).unwrap()
        }
    };

    let result = serde_json::json!({
        "PublicKey": hex::encode_upper(kp.public_key.as_bytes()),
        "Signature": hex::encode_upper(sig.as_bytes()),
    });
    std::fs::write(&out, serde_json::to_vec(&result).unwrap()).unwrap();
    eprintln!("wrote attestation sig to {out}");
}
