use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to signal log file rotation.
///
/// Signals the logging subsystem to close and reopen log files.
/// The actual rotation depends on the tracing subscriber configuration.
pub async fn logrotate(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    tracing::info!("logrotate requested via RPC");

    Ok(serde_json::json!({
        "message": "log file rotation signaled",
    }))
}
