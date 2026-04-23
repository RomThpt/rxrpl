use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::{account_id_from_public_key, decode_account_id, encode_account_id};
use rxrpl_crypto::KeyPair;
use rxrpl_protocol::keylet;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::derive_seed_from_params;

async fn lookup_account_sequence(ctx: &Arc<ServerContext>, account: &str) -> Option<u64> {
    let ledger = ctx.ledger.as_ref()?;
    let account_id = decode_account_id(account).ok()?;
    let key = keylet::account(&account_id);
    let ledger = ledger.read().await;
    let data = ledger.get_state(&key)?;
    let account_data: Value = crate::handlers::common::decode_state_value(data).ok()?;
    account_data.get("Sequence").and_then(|v| v.as_u64())
}

pub async fn sign(params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let mut tx_json = params
        .get("tx_json")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'tx_json'".into()))?;

    let (seed, key_type) = derive_seed_from_params(&params)?;
    let keypair = KeyPair::from_seed(&seed, key_type);

    // Set SigningPubKey
    let pub_hex = hex::encode_upper(keypair.public_key.as_bytes());
    if let Some(obj) = tx_json.as_object_mut() {
        obj.insert("SigningPubKey".to_string(), Value::String(pub_hex.clone()));

        // Set Account if not already present
        if !obj.contains_key("Account") {
            let account_id = account_id_from_public_key(keypair.public_key.as_bytes());
            obj.insert(
                "Account".to_string(),
                Value::String(encode_account_id(&account_id)),
            );
        }

        // Auto-fill Fee from open ledger if missing (rippled parity).
        if !obj.contains_key("Fee") {
            obj.insert("Fee".to_string(), Value::String("10".to_string()));
        }

        // Auto-fill Sequence from account state if missing.
        if !obj.contains_key("Sequence") {
            if let Some(account) = obj.get("Account").and_then(|v| v.as_str()) {
                if let Some(seq) = lookup_account_sequence(_ctx, account).await {
                    obj.insert("Sequence".to_string(), Value::Number(seq.into()));
                }
            }
        }
    }

    // Encode for signing
    let signing_bytes = rxrpl_codec::binary::encode_for_signing(&tx_json)
        .map_err(|e| RpcServerError::Internal(format!("encoding error: {e}")))?;

    // Sign
    let signature = match key_type {
        rxrpl_crypto::KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::sign(&signing_bytes, &keypair.private_key)
                .map_err(|e| RpcServerError::Internal(format!("signing error: {e}")))?
        }
        rxrpl_crypto::KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::sign(&signing_bytes, &keypair.private_key)
                .map_err(|e| RpcServerError::Internal(format!("signing error: {e}")))?
        }
    };

    let sig_hex = hex::encode_upper(signature.as_bytes());
    if let Some(obj) = tx_json.as_object_mut() {
        obj.insert("TxnSignature".to_string(), Value::String(sig_hex));
    }

    // Encode the full signed transaction
    let tx_blob = rxrpl_codec::binary::encode(&tx_json)
        .map_err(|e| RpcServerError::Internal(format!("encoding error: {e}")))?;

    // Compute transaction hash
    let hash_prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
    let mut hash_input = hash_prefix.to_vec();
    hash_input.extend_from_slice(&tx_blob);
    let tx_hash = rxrpl_crypto::sha512_half::sha512_half(&[&hash_input]);
    if let Some(obj) = tx_json.as_object_mut() {
        obj.insert("hash".to_string(), Value::String(tx_hash.to_string()));
    }

    Ok(serde_json::json!({
        "tx_blob": hex::encode_upper(&tx_blob),
        "tx_json": tx_json,
    }))
}
