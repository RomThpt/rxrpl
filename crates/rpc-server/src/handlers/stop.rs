use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Initiate server shutdown.
///
/// Sends a shutdown signal and returns confirmation. The actual
/// shutdown is handled asynchronously by the server runtime.
pub async fn stop(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    tracing::info!("server stop requested via RPC");

    // Spawn a delayed shutdown to allow the response to be sent
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        tracing::info!("shutting down server");
        std::process::exit(0);
    });

    Ok(serde_json::json!({
        "message": "ripple server stopping",
    }))
}
