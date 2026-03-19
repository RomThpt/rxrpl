use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return internal object counts for diagnostics.
///
/// Admin-only method matching rippled's `get_counts` RPC. Returns ledger
/// history depth, transaction queue size, and peer reservation count.
pub async fn get_counts(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger_count = if let Some(ref cl) = ctx.closed_ledgers {
        cl.read().await.len()
    } else {
        0
    };

    let tx_queue_size = if let Some(ref q) = ctx.tx_queue {
        q.read().await.len()
    } else {
        0
    };

    let peer_reservations = ctx.peer_reservations.read().await.len();

    Ok(serde_json::json!({
        "ledger_count": ledger_count,
        "tx_queue_size": tx_queue_size,
        "peer_reservations": peer_reservations,
        "uptime": 0,
    }))
}
