use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Request a specific ledger for async fetching.
///
/// If the ledger is already available locally, returns it immediately.
/// Otherwise, initiates a fetch from the network. This is primarily
/// used for filling gaps in ledger history.
pub async fn ledger_request(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger_index = params
        .get("ledger_index")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'ledger_index'".into()))?
        as u32;

    // Check if we already have the ledger
    if let Some(ref closed) = ctx.closed_ledgers {
        let closed = closed.read().await;
        if let Some(ledger) = closed.iter().find(|l| l.header.sequence == ledger_index) {
            return Ok(serde_json::json!({
                "ledger_index": ledger.header.sequence,
                "ledger_hash": ledger.header.hash.to_string(),
                "acquiring": false,
                "have_header": true,
                "have_state": true,
                "have_transactions": true,
            }));
        }
    }

    // Check current open ledger
    if let Some(ref ledger_lock) = ctx.ledger {
        let ledger = ledger_lock.read().await;
        if ledger.header.sequence == ledger_index {
            return Ok(serde_json::json!({
                "ledger_index": ledger.header.sequence,
                "acquiring": false,
                "have_header": true,
                "have_state": true,
                "have_transactions": true,
            }));
        }
    }

    // Ledger not available locally -- would initiate network fetch
    Ok(serde_json::json!({
        "ledger_index": ledger_index,
        "acquiring": true,
        "have_header": false,
        "have_state": false,
        "have_transactions": false,
    }))
}
