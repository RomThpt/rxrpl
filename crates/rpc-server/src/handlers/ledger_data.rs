use std::sync::Arc;

use serde_json::Value;

use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

pub async fn ledger_data(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(256)
        .min(2048) as usize;

    let binary = params
        .get("binary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let marker: Option<Hash256> = if let Some(m) = params.get("marker").and_then(|v| v.as_str()) {
        Some(
            m.parse()
                .map_err(|e| RpcServerError::InvalidParams(format!("invalid marker: {e}")))?,
        )
    } else {
        None
    };

    let mut state = Vec::new();
    let mut next_marker = None;
    let mut skipping = marker.is_some();

    ledger.state_map.for_each(&mut |key, data| {
        if state.len() >= limit {
            if next_marker.is_none() {
                next_marker = Some(key.to_string());
            }
            return;
        }

        if skipping {
            if *key == marker.unwrap() {
                skipping = false;
            }
            return;
        }

        if binary {
            state.push(serde_json::json!({
                "index": key.to_string(),
                "data": hex::encode(data),
            }));
        } else {
            let node: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap_or(Value::Null);
            state.push(serde_json::json!({
                "index": key.to_string(),
                "data": node,
            }));
        }
    });

    let mut result = serde_json::json!({
        "ledger_index": ledger.header.sequence,
        "ledger_hash": ledger.header.hash.to_string(),
        "state": state,
    });

    if let Some(m) = next_marker {
        result["marker"] = Value::String(m);
    }

    Ok(result)
}
