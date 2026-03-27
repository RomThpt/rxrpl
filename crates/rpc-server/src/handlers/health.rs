use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Server health check endpoint.
///
/// Returns a minimal response indicating whether the server is healthy.
/// Matches rippled's `health` RPC which returns 200 OK when healthy
/// or an error when the server is not in a good state.
pub async fn health(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let has_ledger = ctx.ledger.is_some();
    let has_closed = if let Some(ref cl) = ctx.closed_ledgers {
        !cl.read().await.is_empty()
    } else {
        false
    };

    let server_state = if has_ledger && has_closed {
        "full"
    } else if has_ledger {
        "connected"
    } else {
        "disconnected"
    };

    // rippled returns an error if the server is not synced
    if server_state == "disconnected" {
        return Err(RpcServerError::Server(
            "server is not healthy: disconnected".into(),
        ));
    }

    Ok(serde_json::json!({
        "status": "ok",
        "server_state": server_state,
    }))
}
