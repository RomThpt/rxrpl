use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to manage online deletion threshold.
///
/// With `can_delete` parameter: sets the oldest ledger the server may delete.
///   - A numeric value sets the maximum sequence eligible for deletion.
///   - `"always"` removes the advisory limit (same as u32::MAX).
///   - `"never"` prevents any automatic deletion (same as 0).
/// Without parameters: returns the current earliest available ledger
/// and the advisory delete cursor.
pub async fn can_delete(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    // Handle set operations
    if let Some(val) = params.get("can_delete") {
        if let Some(seq) = val.as_u64() {
            let seq = seq as u32;
            if let Some(ref ps) = ctx.pruner_state {
                ps.can_delete_seq.store(seq, Ordering::Relaxed);
            }
            tracing::info!("can_delete threshold set to {}", seq);
            return Ok(serde_json::json!({ "can_delete": seq }));
        }
        if let Some(s) = val.as_str() {
            match s {
                "always" => {
                    if let Some(ref ps) = ctx.pruner_state {
                        ps.can_delete_seq.store(u32::MAX, Ordering::Relaxed);
                    }
                    tracing::info!("can_delete set to always");
                    return Ok(serde_json::json!({ "can_delete": "always" }));
                }
                "never" => {
                    if let Some(ref ps) = ctx.pruner_state {
                        ps.can_delete_seq.store(0, Ordering::Relaxed);
                    }
                    tracing::info!("can_delete set to never");
                    return Ok(serde_json::json!({ "can_delete": "never" }));
                }
                _ => {
                    if let Ok(seq) = s.parse::<u32>() {
                        if let Some(ref ps) = ctx.pruner_state {
                            ps.can_delete_seq.store(seq, Ordering::Relaxed);
                        }
                        tracing::info!("can_delete threshold set to {}", seq);
                        return Ok(serde_json::json!({ "can_delete": seq }));
                    }
                    return Err(RpcServerError::InvalidParams(
                        "can_delete must be a ledger sequence, 'always', or 'never'".into(),
                    ));
                }
            }
        }
    }

    // Without params: return state
    if let Some(ref ps) = ctx.pruner_state {
        let earliest = ps.earliest_seq.load(Ordering::Relaxed);
        let can_delete = ps.can_delete_seq.load(Ordering::Relaxed);
        let can_delete_val: Value = if can_delete == u32::MAX {
            Value::String("always".into())
        } else if can_delete == 0 && ps.advisory_delete {
            Value::String("never".into())
        } else {
            Value::from(can_delete)
        };
        return Ok(serde_json::json!({
            "can_delete": can_delete_val,
            "earliest_seq": earliest,
        }));
    }

    // Fallback: report from closed ledger history
    let oldest = if let Some(ref cl) = ctx.closed_ledgers {
        let history = cl.read().await;
        history.front().map(|l| l.header.sequence).unwrap_or(0)
    } else {
        0
    };

    Ok(serde_json::json!({ "can_delete": oldest }))
}
