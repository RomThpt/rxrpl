use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn ledger_cleaner(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    tracing::info!("ledger_cleaner requested via RPC");

    Ok(serde_json::json!({
        "message": "Cleaner configured",
    }))
}
