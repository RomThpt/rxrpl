use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn peer_reservations_list(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let reservations: Vec<Value> = ctx
        .peer_reservations
        .read()
        .await
        .iter()
        .map(|pk| serde_json::json!({ "node": pk }))
        .collect();

    Ok(serde_json::json!({
        "reservations": reservations,
    }))
}
