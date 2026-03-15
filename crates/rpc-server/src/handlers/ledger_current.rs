use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn ledger_current(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;

    let ledger = ledger.read().await;

    Ok(serde_json::json!({
        "ledger_current_index": ledger.header.sequence,
    }))
}
