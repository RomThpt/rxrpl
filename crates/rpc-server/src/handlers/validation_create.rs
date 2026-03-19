use std::sync::Arc;

use serde_json::Value;

use rxrpl_crypto::{KeyPair, KeyType, Seed};

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn validation_create(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let seed = if let Some(secret) = params.get("secret").and_then(|v| v.as_str()) {
        Seed::from_passphrase(secret)
    } else {
        Seed::random()
    };

    let keypair = KeyPair::from_seed(&seed, KeyType::Ed25519);

    Ok(serde_json::json!({
        "validation_public_key": hex::encode_upper(keypair.public_key.as_bytes()),
        "validation_seed": hex::encode_upper(seed.as_bytes()),
        "validation_key": hex::encode_upper(keypair.public_key.as_bytes()),
    }))
}
