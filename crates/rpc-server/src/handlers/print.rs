use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to print internal server state for diagnostics.
pub async fn print(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger_seq = if let Some(ref l) = ctx.ledger {
        l.read().await.header.sequence
    } else {
        0
    };

    let queue_size = if let Some(ref q) = ctx.tx_queue {
        q.read().await.len()
    } else {
        0
    };

    let (closed_count, oldest_seq, newest_seq) = if let Some(ref cl) = ctx.closed_ledgers {
        let history = cl.read().await;
        let oldest = history.front().map(|l| l.header.sequence).unwrap_or(0);
        let newest = history.back().map(|l| l.header.sequence).unwrap_or(0);
        (history.len(), oldest, newest)
    } else {
        (0, 0, 0)
    };

    let has_tx_store = ctx.tx_store.is_some();
    let has_relay = ctx.relay_tx.is_some();

    Ok(serde_json::json!({
        "ledger_sequence": ledger_seq,
        "tx_queue_size": queue_size,
        "closed_ledgers": closed_count,
        "oldest_ledger": oldest_seq,
        "newest_ledger": newest_seq,
        "has_tx_store": has_tx_store,
        "has_relay": has_relay,
        "has_metrics": ctx.metrics_handle.is_some(),
        "server_state": if ctx.ledger.is_some() { "full" } else { "disconnected" },
    }))
}
