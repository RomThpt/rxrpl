use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to manage online deletion threshold.
///
/// With `can_delete` parameter: sets the oldest ledger the server may delete.
/// Without parameters: returns the current lowest available ledger.
pub async fn can_delete(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    // For set operations, acknowledge with the requested value
    if let Some(val) = params.get("can_delete") {
        if let Some(seq) = val.as_u64() {
            tracing::info!("can_delete threshold set to {}", seq);
            return Ok(serde_json::json!({ "can_delete": seq }));
        }
        if let Some(s) = val.as_str() {
            match s {
                "always" | "never" => {
                    tracing::info!("can_delete set to {}", s);
                    return Ok(serde_json::json!({ "can_delete": s }));
                }
                _ => {
                    if let Ok(seq) = s.parse::<u64>() {
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

    // Without params: return the oldest available ledger
    let oldest = if let Some(ref cl) = ctx.closed_ledgers {
        let history = cl.read().await;
        history.front().map(|l| l.header.sequence).unwrap_or(0)
    } else {
        0
    };

    Ok(serde_json::json!({ "can_delete": oldest }))
}
