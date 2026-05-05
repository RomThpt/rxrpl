use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return list of connected peers.
///
/// This is a placeholder implementation that returns an empty peer list
/// until the P2P overlay network integration is complete.
pub async fn peers(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "peers": [],
        "peer_count": 0,
    }))
}
