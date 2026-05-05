use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::{account_id_from_public_key, encode_account_id};
use rxrpl_crypto::KeyPair;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::derive_seed_from_params;

/// Add a multisig signature to a transaction.
pub async fn sign_for(params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let mut tx_json = params
        .get("tx_json")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'tx_json'".into()))?;

    let account = params
        .get("account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'account' (signer account)".into()))?
        .to_string();

    let (seed, key_type) = derive_seed_from_params(&params)?;
    let keypair = KeyPair::from_seed(&seed, key_type);
    let pub_hex = hex::encode_upper(keypair.public_key.as_bytes());

    // Verify the account matches the keypair
    let derived_account_id = account_id_from_public_key(keypair.public_key.as_bytes());
    let derived_account_str = encode_account_id(&derived_account_id);
    if derived_account_str != account {
        return Err(RpcServerError::InvalidParams(
            "account does not match the provided secret/seed".into(),
        ));
    }

    // Encode for multisigning (includes the signer account in the hash)
    let signing_bytes =
        rxrpl_codec::binary::encode_for_multisigning(&tx_json, derived_account_id.as_bytes())
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

    // Build new signer entry
    let signer_entry = serde_json::json!({
        "Signer": {
            "Account": account,
            "TxnSignature": sig_hex,
            "SigningPubKey": pub_hex,
        }
    });

    // Append to existing Signers array or create one
    if let Some(obj) = tx_json.as_object_mut() {
        // Ensure SigningPubKey is empty (multisig convention)
        obj.insert("SigningPubKey".to_string(), Value::String(String::new()));

        let signers = obj
            .entry("Signers")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(arr) = signers.as_array_mut() {
            arr.push(signer_entry);
        }
    }

    // Re-serialize the (now multi-signed) tx_json to a hex blob so the
    // caller can submit it directly via `submit_multisigned`. Matches
    // rippled's sign_for response shape.
    let tx_blob = rxrpl_codec::binary::encode(&tx_json)
        .map_err(|e| RpcServerError::Internal(format!("encoding error: {e}")))?;

    Ok(serde_json::json!({
        "tx_blob": hex::encode_upper(&tx_blob),
        "tx_json": tx_json,
    }))
}
