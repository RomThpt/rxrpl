use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to signal log file rotation.
///
/// In production this would signal the logging subsystem to close
/// and reopen log files, allowing external log rotation tools to work.
pub async fn logrotate(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    tracing::info!("logrotate requested via RPC");

    Ok(serde_json::json!({
        "message": "rotate complete",
    }))
}
