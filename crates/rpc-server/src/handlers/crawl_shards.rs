use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return shard information for network crawlers.
///
/// Matches rippled's `crawl_shards` RPC, which returns information
/// about which shards a server stores. Used by network crawlers to
/// map shard distribution across the network.
///
/// This implementation returns empty shard info since the shard store
/// subsystem is not yet implemented.
pub async fn crawl_shards(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "complete_shards": "none",
        "peers": [],
    }))
}
