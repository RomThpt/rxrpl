use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to initiate downloading a shard from the P2P network.
///
/// Matches rippled's `download_shard` RPC. Validates the request, marks the
/// requested shards as `Downloading` in the shard manager, and returns an
/// acknowledgment. The actual download is driven by the overlay's ShardSyncer
/// which periodically checks for shards in the `Downloading` state.
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

    // Validate each shard entry has required fields.
    let mut indices = Vec::new();
    for shard in shards {
        let index = shard.get("index").and_then(|v| v.as_u64()).ok_or_else(|| {
            RpcServerError::InvalidParams("each shard must have a numeric 'index'".into())
        })? as u32;
        // URL is optional for P2P downloads but still accepted for API compatibility.
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
    let mut already_complete = Vec::new();
    let mut queued = Vec::new();

    for &index in &indices {
        if manager.is_complete(index) {
            already_complete.push(index);
        } else {
            manager.mark_downloading(index);
            queued.push(index);
        }
    }

    Ok(serde_json::json!({
        "message": "downloading shards",
        "status": "acknowledged",
        "shard_indices": queued,
        "already_complete": already_complete,
    }))
}
