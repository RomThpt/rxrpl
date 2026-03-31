use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return shard information for network crawlers.
///
/// Matches rippled's `crawl_shards` RPC, which returns information
/// about which shards a server stores. Used by network crawlers to
/// map shard distribution across the network.
pub async fn crawl_shards(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let Some(ref manager_lock) = ctx.shard_manager else {
        return Ok(serde_json::json!({
            "complete_shards": "none",
            "incomplete_shards": [],
            "peers": [],
        }));
    };

    let manager = manager_lock.read().await;
    let complete = manager.complete_shards_string();
    let incomplete: Vec<Value> = manager
        .incomplete_shard_info()
        .iter()
        .map(|(idx, count)| {
            serde_json::json!({
                "index": idx,
                "stored_count": count,
            })
        })
        .collect();

    // Peer shard info would be populated from the ShardSyncer if available.
    // Currently the RPC server does not have direct access to the overlay's
    // ShardSyncer, so we return an empty peer list. A future enhancement
    // could pass peer shard summaries through the server event channel.

    Ok(serde_json::json!({
        "complete_shards": complete,
        "incomplete_shards": incomplete,
        "peers": [],
    }))
}
