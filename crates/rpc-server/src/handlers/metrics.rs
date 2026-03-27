use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to return server metrics in Prometheus format.
///
/// Exposes the internal metrics collected by the server. When the
/// `format` parameter is `"prometheus"` (default), returns the raw
/// Prometheus text exposition format. When `"json"`, returns a
/// JSON summary of key metrics.
pub async fn metrics(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    match format {
        "prometheus" => {
            let rendered = if let Some(ref handle) = ctx.metrics_handle {
                handle.render()
            } else {
                String::from("# no metrics available\n")
            };
            Ok(serde_json::json!({
                "format": "prometheus",
                "metrics": rendered,
            }))
        }
        "json" => {
            let ledger_seq = if let Some(ref l) = ctx.ledger {
                l.read().await.header.sequence
            } else {
                0
            };

            let closed_count = if let Some(ref cl) = ctx.closed_ledgers {
                cl.read().await.len()
            } else {
                0
            };

            let tx_queue_size = if let Some(ref q) = ctx.tx_queue {
                q.read().await.len()
            } else {
                0
            };

            Ok(serde_json::json!({
                "format": "json",
                "metrics": {
                    "current_ledger": ledger_seq,
                    "closed_ledger_count": closed_count,
                    "tx_queue_size": tx_queue_size,
                    "peer_reservations": ctx.peer_reservations.read().await.len(),
                },
            }))
        }
        _ => Err(RpcServerError::InvalidParams(format!(
            "invalid format: {format}. Must be 'json' or 'prometheus'"
        ))),
    }
}
