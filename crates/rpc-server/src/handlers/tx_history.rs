use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return recent transactions from closed ledgers.
pub async fn tx_history(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let start = params.get("start").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

    let closed = ctx
        .closed_ledgers
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;

    let closed = closed.read().await;

    let mut txns: Vec<Value> = Vec::new();
    let max_count = 20usize;

    // Iterate closed ledgers in reverse (most recent first)
    let mut skipped = 0usize;
    'outer: for ledger in closed.iter().rev() {
        for (hash, data) in ledger.tx_map.iter() {
            if skipped < start {
                skipped += 1;
                continue;
            }
            if txns.len() >= max_count {
                break 'outer;
            }
            if let Ok(mut record) = serde_json::from_slice::<Value>(&data) {
                if let Some(obj) = record.as_object_mut() {
                    obj.insert("hash".to_string(), Value::String(hash.to_string()));
                    obj.insert(
                        "ledger_index".to_string(),
                        Value::Number(ledger.header.sequence.into()),
                    );
                }
                txns.push(record);
            }
        }
    }

    Ok(serde_json::json!({
        "index": start,
        "txs": txns,
    }))
}
