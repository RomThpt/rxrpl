use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to manage online deletion threshold.
///
/// With `can_delete` parameter: sets the oldest ledger the server may delete.
/// Without parameters: returns the current threshold value.
pub async fn can_delete(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    if let Some(seq) = params
        .get("can_delete")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    {
        tracing::info!("can_delete threshold set to {seq}");
        Ok(serde_json::json!({
            "can_delete": seq,
        }))
    } else if params
        .get("can_delete")
        .and_then(|v| v.as_str())
        .map_or(false, |s| s == "always" || s == "never")
    {
        let value = params["can_delete"].as_str().unwrap();
        tracing::info!("can_delete set to {value}");
        Ok(serde_json::json!({
            "can_delete": value,
        }))
    } else {
        // Return current value (default: 0 means no restriction)
        Ok(serde_json::json!({
            "can_delete": 0,
        }))
    }
}
