use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to return peer crawler information.
///
/// Returns information about connected peers for network crawlers,
/// including uptime, version, and connection details.
pub async fn crawl(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    // Placeholder: returns empty overlay info until P2P integration is complete.
    Ok(serde_json::json!({
        "overlay": {
            "active": [],
        },
        "server": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": "full",
            "uptime": 0,
        },
    }))
}
