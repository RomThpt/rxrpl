use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::{account_id_from_public_key, encode_account_id};
use rxrpl_codec::address::seed::encode_seed;
use rxrpl_crypto::{KeyPair, KeyType, Seed};

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn wallet_propose(
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

    let seed = if let Some(passphrase) = params.get("passphrase").and_then(|v| v.as_str()) {
        Seed::from_passphrase(passphrase)
    } else {
        Seed::random()
    };

    let keypair = KeyPair::from_seed(&seed, key_type);
    let account_id = account_id_from_public_key(keypair.public_key.as_bytes());
    let account_address = encode_account_id(&account_id);

    let master_seed = encode_seed(seed.as_bytes(), key_type)
        .map_err(|e| RpcServerError::Internal(format!("seed encoding error: {e}")))?;

    Ok(serde_json::json!({
        "master_seed": master_seed,
        "master_seed_hex": hex::encode_upper(seed.as_bytes()),
        "account_id": account_address,
        "public_key": hex::encode_upper(keypair.public_key.as_bytes()),
        "public_key_hex": hex::encode_upper(keypair.public_key.as_bytes()),
        "key_type": key_type_str,
    }))
}
