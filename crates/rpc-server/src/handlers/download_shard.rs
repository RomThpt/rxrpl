use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to initiate downloading a shard from a URL.
///
/// Matches rippled's `download_shard` RPC. In rippled this instructs
/// the server to download ledger history from an archive site and
/// import it into the shard store.
///
/// This implementation returns a stub acknowledging the request since
/// the shard store subsystem is not yet implemented.
pub async fn download_shard(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let shards = params
        .get("shards")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'shards' array".into()))?;

    if shards.is_empty() {
        return Err(RpcServerError::InvalidParams(
            "'shards' array must not be empty".into(),
        ));
    }

    // Validate each shard entry has required fields
    for shard in shards {
        shard.get("index").and_then(|v| v.as_u64()).ok_or_else(|| {
            RpcServerError::InvalidParams("each shard must have a numeric 'index'".into())
        })?;
        shard.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcServerError::InvalidParams("each shard must have a 'url' string".into())
        })?;
    }

    Ok(serde_json::json!({
        "message": "downloading shards",
        "shards": shards,
    }))
}
