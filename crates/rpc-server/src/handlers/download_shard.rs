use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to initiate downloading a shard from a URL.
///
/// Matches rippled's `download_shard` RPC. Marks the requested shards as
/// `Downloading` in the shard manager.
pub async fn download_shard(
    params: Value,
    ctx: &Arc<ServerContext>,
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
    let mut indices = Vec::new();
    for shard in shards {
        let index = shard.get("index").and_then(|v| v.as_u64()).ok_or_else(|| {
            RpcServerError::InvalidParams("each shard must have a numeric 'index'".into())
        })? as u32;
        shard.get("url").and_then(|v| v.as_str()).ok_or_else(|| {
            RpcServerError::InvalidParams("each shard must have a 'url' string".into())
        })?;
        indices.push(index);
    }

    let Some(ref manager_lock) = ctx.shard_manager else {
        return Ok(serde_json::json!({
            "message": "downloading shards",
            "status": "shard_store_not_enabled",
            "shards": shards,
        }));
    };

    let mut manager = manager_lock.write().await;
    for index in &indices {
        manager.mark_downloading(*index);
    }

    Ok(serde_json::json!({
        "message": "downloading shards",
        "status": "acknowledged",
        "shard_indices": indices,
    }))
}
