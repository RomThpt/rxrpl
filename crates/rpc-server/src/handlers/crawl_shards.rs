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
            "peers": [],
        }));
    };

    let manager = manager_lock.read().await;
    let complete = manager.complete_shards_string();

    Ok(serde_json::json!({
        "complete_shards": complete,
        "peers": [],
    }))
}
