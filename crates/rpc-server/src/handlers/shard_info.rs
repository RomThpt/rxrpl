use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to return shard store status.
///
/// Matches rippled's `shard_info` / `crawl_shards` RPC. Returns information
/// about locally stored shards of ledger history.
///
/// This implementation returns empty shard info since the shard store
/// subsystem is not yet implemented.
pub async fn shard_info(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "shards": "none",
        "info": {
            "complete_shards": "none",
            "finalized": false,
        },
    }))
}
