use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return server version information.
pub async fn version(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "version": {
            "server_version": env!("CARGO_PKG_VERSION"),
            "first_protocol_version": 1,
            "last_protocol_version": 2,
            "build_date": env!("CARGO_PKG_VERSION"),
        }
    }))
}
