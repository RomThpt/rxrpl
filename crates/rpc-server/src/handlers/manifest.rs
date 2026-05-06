use std::sync::Arc;

use base64::Engine;
use serde_json::{Value, json};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the validator manifest for a given public key.
///
/// Manifests bind a validator's ephemeral signing key to its master
/// public key. Currently this only resolves the **local** manifest (the
/// one this node publishes for itself). Lookup of peer manifests will
/// land alongside the ManifestStore-mirror plumbing in a follow-up.
pub async fn manifest(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let public_key = params
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'public_key'".into()))?;

    let requested_bytes = decode_pubkey(public_key);

    if let Some(snapshot) = ctx.local_manifest() {
        let matches = match requested_bytes.as_deref() {
            Some(bytes) => {
                bytes == snapshot.master_public_key.as_slice()
                    || bytes == snapshot.ephemeral_public_key.as_slice()
            }
            None => false,
        };
        if matches {
            let manifest_b64 =
                base64::engine::general_purpose::STANDARD.encode(&snapshot.raw_bytes);
            return Ok(json!({
                "requested": public_key,
                "details": {
                    "master_key": hex::encode(&snapshot.master_public_key),
                    "signing_key": hex::encode(&snapshot.ephemeral_public_key),
                    "seq": snapshot.sequence,
                    "domain": snapshot.domain.clone().unwrap_or_default(),
                },
                "manifest": manifest_b64,
            }));
        }
    }

    Ok(json!({
        "requested": public_key,
        "details": {
            "master_key": public_key,
            "seq": Value::Null,
        },
        "manifest": Value::Null,
    }))
}

/// Decode a public-key parameter into raw bytes. Accepts hex (66 chars,
/// the typical representation in rippled responses) or base58 (`n...`,
/// rippled config form). Returns `None` on malformed input — the caller
/// resolves "no manifest" rather than erroring out.
fn decode_pubkey(s: &str) -> Option<Vec<u8>> {
    let trimmed = s.trim();
    if trimmed.len() == 66 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return hex::decode(trimmed).ok();
    }
    if trimmed.starts_with('n') {
        // Node public-key prefix per rippled (`TokenType::NodePublic = 0x1C`).
        if let Ok(bytes) = rxrpl_codec::address::base58::base58check_decode(trimmed) {
            // Strip the 1-byte version prefix.
            if bytes.len() == 34 && bytes[0] == 0x1C {
                return Some(bytes[1..].to_vec());
            }
        }
    }
    None
}
