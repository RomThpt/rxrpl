use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn tx(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let hash_str = params
        .get("transaction")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'transaction' field".into()))?;

    let hash = Hash256::from_str(hash_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid hash: {e}")))?;

    // Search open ledger tx_map
    if let Some(ref ledger) = ctx.ledger {
        let ledger = ledger.read().await;
        if let Some(data) = ledger.tx_map.get(&hash) {
            let record: Value = serde_json::from_slice(data)
                .map_err(|e| RpcServerError::Internal(format!("failed to deserialize tx: {e}")))?;
            return Ok(record);
        }
    }

    // Search closed ledgers
    if let Some(ref closed) = ctx.closed_ledgers {
        let closed = closed.read().await;
        for ledger in closed.iter().rev() {
            if let Some(data) = ledger.tx_map.get(&hash) {
                let record: Value = serde_json::from_slice(data).map_err(|e| {
                    RpcServerError::Internal(format!("failed to deserialize tx: {e}"))
                })?;
                return Ok(record);
            }
        }
    }

    Err(RpcServerError::InvalidParams("transaction not found".into()))
}
