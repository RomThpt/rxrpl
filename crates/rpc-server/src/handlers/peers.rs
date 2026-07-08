use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return list of connected peers.
///
/// Reports the live overlay connection count. The per-peer detail array is not
/// yet surfaced through the context, so `peers` stays empty while `peer_count`
/// reflects the real overlay state.
pub async fn peers(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "peers": [],
        "peer_count": ctx.peer_count(),
    }))
}
