use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to control transaction relay load.
///
/// The flag is accepted and echoed back, but the overlay does not yet expose a
/// backing knob to act on it, so setting it currently has no effect on relay
/// volume.
pub async fn tx_reduce_relay(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let enable = params
        .get("tx_reduce_relay")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    tracing::info!("tx_reduce_relay set to {}", enable);

    Ok(serde_json::json!({
        "tx_reduce_relay": enable,
    }))
}
