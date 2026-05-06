use std::sync::Arc;

use base64::Engine;
use serde_json::{Value, json};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Sentinel sequence indicating an explicit revocation manifest.
/// Mirrors `rxrpl_overlay::manifest::MANIFEST_REVOKED_SEQ` to avoid a
/// cross-crate dependency just for one constant.
const MANIFEST_REVOKED_SEQ: u32 = u32::MAX;

/// Rippled `TokenType::NodePublic` prefix used when encoding validator
/// public keys as `n…` base58 strings.
const NODE_PUBLIC_KEY_PREFIX: &[u8] = &[0x1C];

/// Return introspection on this node's validator identity, if any.
///
/// Surfaces only public material (master/signing keys, sequence, domain,
/// signed manifest blob). Seeds and private keys are never read here.
pub async fn validator_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let Some(snapshot) = ctx.local_manifest() else {
        return Ok(json!({ "validator": { "status": "not_configured" } }));
    };

    let status = if snapshot.sequence == MANIFEST_REVOKED_SEQ {
        "revoked"
    } else {
        "active"
    };

    let master_n = rxrpl_codec::address::base58::base58check_encode(
        &snapshot.master_public_key,
        NODE_PUBLIC_KEY_PREFIX,
    );
    let signing_n = rxrpl_codec::address::base58::base58check_encode(
        &snapshot.ephemeral_public_key,
        NODE_PUBLIC_KEY_PREFIX,
    );
    let manifest_b64 = base64::engine::general_purpose::STANDARD.encode(&snapshot.raw_bytes);

    let mut validator = json!({
        "status": status,
        "master_key": hex::encode(&snapshot.master_public_key),
        "master_key_n": master_n,
        "signing_key": hex::encode(&snapshot.ephemeral_public_key),
        "signing_key_n": signing_n,
        "seq": snapshot.sequence,
        "manifest": manifest_b64,
        "last_rotated_unix": snapshot.last_rotated_unix,
    });
    if let Some(domain) = snapshot.domain.as_deref() {
        validator["domain"] = Value::String(domain.to_string());
    }

    Ok(json!({ "validator": validator }))
}
