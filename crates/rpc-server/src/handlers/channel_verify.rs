use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_crypto::KeyType;
use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn channel_verify(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let channel_id_str = params
        .get("channel_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'channel_id'".into()))?;

    let channel_id = Hash256::from_str(channel_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid channel_id: {e}")))?;

    let amount_str = params
        .get("amount")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'amount'".into()))?;

    let amount: u64 = amount_str
        .parse()
        .map_err(|_| RpcServerError::InvalidParams("invalid amount".into()))?;

    let signature_hex = params
        .get("signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'signature'".into()))?;

    let public_key_hex = params
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'public_key'".into()))?;

    let signature_bytes = hex::decode(signature_hex)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid signature hex: {e}")))?;

    let public_key_bytes = hex::decode(public_key_hex)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid public_key hex: {e}")))?;

    // Build the claim message
    let prefix = HashPrefix::PAYMENT_CHANNEL_CLAIM.to_bytes();
    let mut message = Vec::with_capacity(4 + 32 + 8);
    message.extend_from_slice(&prefix);
    message.extend_from_slice(channel_id.as_bytes());
    message.extend_from_slice(&amount.to_be_bytes());

    let key_type = KeyType::from_public_key(&public_key_bytes).ok_or_else(|| {
        RpcServerError::InvalidParams("cannot determine key type from public key".into())
    })?;

    let verified = match key_type {
        KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::verify(&message, &public_key_bytes, &signature_bytes)
        }
        KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::verify(&message, &public_key_bytes, &signature_bytes)
        }
    };

    Ok(serde_json::json!({
        "signature_verified": verified,
    }))
}
