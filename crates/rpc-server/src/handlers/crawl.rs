use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::server_info::format_ledger_ranges;

/// Admin command to return peer crawler information.
///
/// Returns server metadata for network crawlers. Peer connection
/// details require overlay integration; this returns the server portion.
pub async fn crawl(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger_seq = if let Some(ref l) = ctx.ledger {
        l.read().await.header.sequence
    } else {
        0
    };

    let (closed_count, complete_ledgers) = if let Some(ref cl) = ctx.closed_ledgers {
        let guard = cl.read().await;
        let mut seqs: Vec<u32> = guard.iter().map(|l| l.header.sequence).collect();
        seqs.sort_unstable();
        (guard.len(), format_ledger_ranges(&seqs))
    } else {
        (0, "empty".to_string())
    };

    let queue_size = if let Some(ref q) = ctx.tx_queue {
        q.read().await.len()
    } else {
        0
    };

    let reservations = ctx.peer_reservations.read().await;

    Ok(serde_json::json!({
        "overlay": {
            "active": [],
        },
        "server": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": if ctx.ledger.is_some() { "full" } else { "disconnected" },
            "complete_ledgers": complete_ledgers,
            "uptime": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        },
        "counts": {
            "ledger_sequence": ledger_seq,
            "closed_ledgers": closed_count,
            "tx_queue_size": queue_size,
            "peer_reservations": reservations.len(),
        },
    }))
}
