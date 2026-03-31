use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn ledger_closed(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    // In reporting mode, query the ledger store for the latest header
    if ctx.reporting_mode {
        if let Some(ref store) = ctx.ledger_store {
            let seq = store
                .latest_sequence()
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                .ok_or_else(|| RpcServerError::Internal("no ledger data available yet".into()))?;

            let record = store
                .get_ledger_header(seq)
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                .ok_or_else(|| RpcServerError::Internal("ledger header not found".into()))?;

            return Ok(serde_json::json!({
                "ledger_hash": hex::encode(&record.hash),
                "ledger_index": record.sequence,
            }));
        }
        return Err(RpcServerError::Internal(
            "reporting mode has no ledger store".into(),
        ));
    }

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
