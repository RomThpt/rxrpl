use std::sync::Arc;

use serde_json::Value;

use rxrpl_crypto::{KeyPair, KeyType, Seed};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to set the validation key from a given seed.
///
/// Takes a `secret` parameter and derives the validation keypair.
/// Similar to `validation_create` but always requires a seed input.
pub async fn validation_seed(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let secret = params
        .get("secret")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'secret' field".into()))?;

    let seed = Seed::from_passphrase(secret);
    let keypair = KeyPair::from_seed(&seed, KeyType::Ed25519);

    tracing::info!("validation seed set via RPC");

    Ok(serde_json::json!({
        "validation_key": hex::encode_upper(keypair.public_key.as_bytes()),
        "validation_public_key": hex::encode_upper(keypair.public_key.as_bytes()),
        "validation_seed": hex::encode_upper(seed.as_bytes()),
    }))
}
