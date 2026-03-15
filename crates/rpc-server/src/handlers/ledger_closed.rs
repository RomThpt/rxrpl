use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn ledger_closed(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let closed = ctx
        .closed_ledgers
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;

    let closed = closed.read().await;
    let ledger = closed
        .back()
        .ok_or_else(|| RpcServerError::Internal("no closed ledger yet".into()))?;

    Ok(serde_json::json!({
        "ledger_hash": ledger.header.hash.to_string(),
        "ledger_index": ledger.header.sequence,
    }))
}
