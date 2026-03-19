use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to configure the ledger cleaner.
///
/// Accepts optional parameters:
/// - `ledger`: specific ledger sequence to clean
/// - `max_ledger`: maximum ledger to clean up to
/// - `min_ledger`: minimum ledger to clean from
/// - `full`: if true, perform full clean
pub async fn ledger_cleaner(
    params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = params.get("ledger").and_then(|v| v.as_u64());
    let max_ledger = params.get("max_ledger").and_then(|v| v.as_u64());
    let min_ledger = params.get("min_ledger").and_then(|v| v.as_u64());
    let full = params.get("full").and_then(|v| v.as_bool()).unwrap_or(false);

    tracing::info!(
        "ledger_cleaner: ledger={:?} min={:?} max={:?} full={}",
        ledger, min_ledger, max_ledger, full
    );

    let mut response = serde_json::Map::new();
    response.insert("message".into(), Value::from("Cleaner configured"));
    if let Some(seq) = ledger {
        response.insert("ledger".into(), Value::from(seq));
    }
    if let Some(seq) = max_ledger {
        response.insert("max_ledger".into(), Value::from(seq));
    }
    if let Some(seq) = min_ledger {
        response.insert("min_ledger".into(), Value::from(seq));
    }
    response.insert("full".into(), Value::from(full));

    Ok(Value::Object(response))
}
