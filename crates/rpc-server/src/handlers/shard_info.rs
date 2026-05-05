use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to return shard store status.
///
/// Matches rippled's `shard_info` RPC. Returns information about locally
/// stored shards of ledger history including their state and sequence ranges.
pub async fn shard_info(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let Some(ref manager_lock) = ctx.shard_manager else {
        return Ok(serde_json::json!({
            "shards": "none",
            "info": {
                "complete_shards": "none",
                "finalized": false,
            },
        }));
    };

    let manager = manager_lock.read().await;
    let shards: Vec<Value> = manager
        .all_shards()
        .iter()
        .map(|s| {
            serde_json::json!({
                "index": s.index,
                "state": format!("{:?}", s.state),
                "first_seq": s.first_seq,
                "last_seq": s.last_seq,
                "last_hash": s.last_hash.map(|h| h.to_string()),
            })
        })
        .collect();

    let complete = manager.complete_shards_string();

    Ok(serde_json::json!({
        "shards": shards,
        "info": {
            "complete_shards": complete,
            "finalized": false,
        },
    }))
}
