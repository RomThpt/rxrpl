use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the validator manifest for a given public key.
///
/// Manifests bind a validator's ephemeral signing key to its
/// master public key. Returns the manifest if found.
pub async fn manifest(params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let public_key = params
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'public_key'".into()))?;

    // Manifest lookup is not yet implemented; return a not-found result
    // rather than an error, matching rippled behavior.
    Ok(serde_json::json!({
        "requested": public_key,
        "details": {
            "master_key": public_key,
            "seq": Value::Null,
        },
        "manifest": Value::Null,
    }))
}
