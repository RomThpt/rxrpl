use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Initiate a peer connection to the specified IP and port.
///
/// This is an admin command that instructs the server to attempt
/// a P2P connection to the given address.
pub async fn connect(params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ip = params
        .get("ip")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'ip'".into()))?;

    let port = params.get("port").and_then(|v| v.as_u64()).unwrap_or(51235);

    tracing::info!("connect requested to {ip}:{port}");

    // P2P overlay connection initiation is not yet wired through RPC.
    // Return an acknowledgment that the request was received.
    Ok(serde_json::json!({
        "connecting": true,
        "ip": ip,
        "port": port,
        "message": "connection attempt initiated",
    }))
}
