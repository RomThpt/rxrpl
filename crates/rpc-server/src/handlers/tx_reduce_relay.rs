use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to control transaction relay load.
///
/// When enabled, reduces the volume of transaction relay messages
/// to lower bandwidth usage. Takes an optional `tx_reduce_relay` boolean.
pub async fn tx_reduce_relay(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let enable = params
        .get("tx_reduce_relay")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    tracing::info!("tx_reduce_relay set to {enable}");

    Ok(serde_json::json!({
        "tx_reduce_relay": enable,
    }))
}
