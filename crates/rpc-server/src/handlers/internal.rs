use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin-only internal debugging command.
///
/// Matches rippled's `internal` RPC which exposes various internal
/// subsystem commands for debugging. The `internal_command` parameter
/// selects the subsystem to query.
///
/// Available subcommands:
/// - `ledger_master` - Return ledger master info
/// - `txq` - Return transaction queue diagnostics
pub async fn internal(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let command = params
        .get("internal_command")
        .and_then(|v| v.as_str())
        .unwrap_or("info");

    match command {
        "ledger_master" => {
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

            Ok(serde_json::json!({
                "internal_command": "ledger_master",
                "info": {
                    "current_ledger": ledger_seq,
                    "closed_ledger_count": closed_count,
                },
            }))
        }
        "txq" => {
            let queue_size = if let Some(ref q) = ctx.tx_queue {
                q.read().await.len()
            } else {
                0
            };

            Ok(serde_json::json!({
                "internal_command": "txq",
                "info": {
                    "queue_size": queue_size,
                },
            }))
        }
        "info" => Ok(serde_json::json!({
            "internal_command": "info",
            "available_commands": ["ledger_master", "txq"],
        })),
        _ => Err(RpcServerError::InvalidParams(format!(
            "unknown internal_command: {command}"
        ))),
    }
}
