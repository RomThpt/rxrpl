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

    let hash = Hash256::from_str(hash_str).map_err(|_| RpcServerError::NotImplemented)?;

    // In reporting mode, query the ledger store directly
    if ctx.reporting_mode {
        if let Some(ref store) = ctx.ledger_store {
            let hash_bytes = hash.as_bytes().to_vec();
            let record = store
                .get_tx(&hash_bytes)
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                .ok_or_else(|| {
                    RpcServerError::InvalidParams("transaction not found".into())
                })?;

            let tx_json: Value =
                serde_json::from_slice(&record.tx_blob).unwrap_or(Value::Null);
            let meta: Value =
                serde_json::from_slice(&record.meta_blob).unwrap_or(Value::Null);

            return Ok(serde_json::json!({
                "hash": hash_str,
                "ledger_index": record.ledger_seq,
                "tx": tx_json,
                "meta": meta,
                "validated": true,
            }));
        }
        return Err(RpcServerError::Internal(
            "reporting mode has no ledger store".into(),
        ));
    }

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

    Err(RpcServerError::InvalidParams(
        "transaction not found".into(),
    ))
}
