use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn peer_reservations_del(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let public_key = params
        .get("public_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'public_key'".into()))?;

    let removed = ctx.peer_reservations.write().await.remove(public_key);
    if !removed {
        return Err(RpcServerError::InvalidParams(
            "reservation not found".into(),
        ));
    }

    Ok(serde_json::json!({
        "previous": { "public_key": public_key },
    }))
}
