use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_crypto::KeyPair;
use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::derive_seed_from_params;

pub async fn channel_authorize(
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

    let (seed, key_type) = derive_seed_from_params(&params)?;
    let keypair = KeyPair::from_seed(&seed, key_type);

    // Build the claim message: CLM prefix + channel_id + amount (big-endian u64)
    let prefix = HashPrefix::PAYMENT_CHANNEL_CLAIM.to_bytes();
    let mut message = Vec::with_capacity(4 + 32 + 8);
    message.extend_from_slice(&prefix);
    message.extend_from_slice(channel_id.as_bytes());
    message.extend_from_slice(&amount.to_be_bytes());

    let signature = match key_type {
        rxrpl_crypto::KeyType::Secp256k1 => {
            rxrpl_crypto::secp256k1::sign(&message, &keypair.private_key)
                .map_err(|e| RpcServerError::Internal(format!("signing error: {e}")))?
        }
        rxrpl_crypto::KeyType::Ed25519 => {
            rxrpl_crypto::ed25519::sign(&message, &keypair.private_key)
                .map_err(|e| RpcServerError::Internal(format!("signing error: {e}")))?
        }
    };

    Ok(serde_json::json!({
        "signature": hex::encode_upper(signature.as_bytes()),
    }))
}
