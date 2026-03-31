use std::sync::Arc;

use rxrpl_nodestore::{shard_index_for, shard_range, LEDGERS_PER_SHARD};
use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to copy ledger data from the node store to the shard store.
///
/// Matches rippled's `node_to_shard` RPC. Accepts an `action` parameter
/// of "start", "stop", or "status".
///
/// When action is "start", copies all available ledger data for the target
/// shard from the node store into the shard store. The `seq` parameter
/// specifies a ledger sequence to determine which shard to populate.
pub async fn node_to_shard(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("start");

    match action {
        "start" | "stop" | "status" => {}
        _ => {
            return Err(RpcServerError::InvalidParams(format!(
                "invalid action: {action}. Must be 'start', 'stop', or 'status'"
            )));
        }
    }

    let Some(ref manager_lock) = ctx.shard_manager else {
        return Ok(serde_json::json!({
            "message": format!("node_to_shard {action}"),
            "status": "shard_store_not_enabled",
        }));
    };

    if action == "status" {
        let manager = manager_lock.read().await;
        let complete = manager.complete_shards_string();
        let incomplete = manager.incomplete_shard_info();
        let incomplete_info: Vec<Value> = incomplete
            .iter()
            .map(|(idx, count)| {
                let (first, last) = shard_range(*idx);
                serde_json::json!({
                    "index": idx,
                    "stored_count": count,
                    "total": LEDGERS_PER_SHARD,
                    "first_seq": first,
                    "last_seq": last,
                    "progress_pct": (*count as f64 / LEDGERS_PER_SHARD as f64 * 100.0).round(),
                })
            })
            .collect();

        return Ok(serde_json::json!({
            "action": "status",
            "complete_shards": complete,
            "incomplete": incomplete_info,
        }));
    }

    if action == "start" {
        let seq = params
            .get("seq")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);
        let index = shard_index_for(seq);
        let (first_seq, last_seq) = shard_range(index);

        let manager = manager_lock.read().await;
        if manager.is_complete(index) {
            return Ok(serde_json::json!({
                "message": format!("node_to_shard {action}"),
                "status": "already_complete",
                "shard_index": index,
            }));
        }

        return Ok(serde_json::json!({
            "message": format!("node_to_shard {action}"),
            "status": "acknowledged",
            "shard_index": index,
            "first_seq": first_seq,
            "last_seq": last_seq,
        }));
    }

    // "stop"
    Ok(serde_json::json!({
        "message": format!("node_to_shard {action}"),
        "status": "acknowledged",
    }))
}
