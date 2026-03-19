use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the minimum and maximum ledger sequences available.
pub async fn ledger_range(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let closed = ctx
        .closed_ledgers
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;

    let closed = closed.read().await;

    if closed.is_empty() {
        return Ok(serde_json::json!({
            "ledger_index_min": Value::Null,
            "ledger_index_max": Value::Null,
        }));
    }

    let min_seq = closed.front().unwrap().header.sequence;
    let max_seq = closed.back().unwrap().header.sequence;

    Ok(serde_json::json!({
        "ledger_index_min": min_seq,
        "ledger_index_max": max_seq,
    }))
}
