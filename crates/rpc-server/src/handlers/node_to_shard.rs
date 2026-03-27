use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to copy ledger data from the node store to the shard store.
///
/// Matches rippled's `node_to_shard` RPC. In rippled this copies validated
/// ledger data from the main node store into the shard store for historical
/// archival.
///
/// This implementation returns a stub response since the shard store
/// subsystem is not yet implemented.
pub async fn node_to_shard(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("start");

    match action {
        "start" | "stop" | "status" => {}
        _ => {
            return Err(RpcServerError::InvalidParams(format!(
                "invalid action: {action}. Must be 'start', 'stop', or 'status'"
            )));
        }
    }

    Ok(serde_json::json!({
        "message": format!("node_to_shard {action}"),
        "status": "not_implemented",
    }))
}
