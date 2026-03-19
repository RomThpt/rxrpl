use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn peer_reservations_add(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let public_key = params
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'public_key'".into()))?;

    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    ctx.peer_reservations
        .write()
        .await
        .insert(public_key.to_string());

    Ok(serde_json::json!({
        "previous": Value::Null,
        "public_key": public_key,
        "description": description,
    }))
}
