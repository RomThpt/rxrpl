use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::seed::encode_seed;
use rxrpl_crypto::{KeyType, Seed};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Deprecated admin command that generates a wallet seed.
///
/// This is the deprecated predecessor of `wallet_propose`. It returns
/// a randomly generated seed. New callers should use `wallet_propose`
/// instead, which also derives the keypair and account address.
pub async fn wallet_seed(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let key_type_str = params
        .get("key_type")
        .and_then(|v| v.as_str())
        .unwrap_or("secp256k1");

    let key_type = match key_type_str {
        "secp256k1" => KeyType::Secp256k1,
        "ed25519" => KeyType::Ed25519,
        _ => {
            return Err(RpcServerError::InvalidParams(format!(
                "invalid key_type: {key_type_str}"
            )));
        }
    };

    let seed = Seed::random();
    let seed_hex = hex::encode(seed.as_bytes());
    let seed_encoded = encode_seed(seed.as_bytes(), key_type)
        .map_err(|e| RpcServerError::Internal(format!("seed encoding error: {e}")))?;

    Ok(serde_json::json!({
        "deprecated": "Use wallet_propose instead.",
        "seed": seed_encoded,
        "seed_hex": seed_hex,
        "key_type": key_type_str,
    }))
}
