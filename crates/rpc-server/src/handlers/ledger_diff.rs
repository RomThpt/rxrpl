use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn ledger_diff(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let base_seq: u32 = params
        .get("ledger_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'ledger_index'".into()))?;

    let diff_seq: u32 = params
        .get("diff_ledger_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32)
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'diff_ledger_index'".into()))?;

    let closed = ctx
        .closed_ledgers
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;
    let history = closed.read().await;

    let base = history
        .iter()
        .find(|l| l.header.sequence == base_seq)
        .ok_or_else(|| RpcServerError::InvalidParams(format!("ledger {base_seq} not found")))?;

    let diff = history
        .iter()
        .find(|l| l.header.sequence == diff_seq)
        .ok_or_else(|| RpcServerError::InvalidParams(format!("ledger {diff_seq} not found")))?;

    let mut differences = Vec::new();

    // Collect all keys from base ledger
    let mut base_keys = HashMap::new();
    base.state_map.for_each(&mut |key, data| {
        base_keys.insert(*key, data.to_vec());
    });

    // Compare with diff ledger
    diff.state_map
        .for_each(&mut |key, data| match base_keys.remove(key) {
            Some(base_data) => {
                if base_data != data {
                    differences.push(serde_json::json!({
                        "type": "modified",
                        "index": key.to_string(),
                    }));
                }
            }
            None => {
                differences.push(serde_json::json!({
                    "type": "created",
                    "index": key.to_string(),
                }));
            }
        });

    // Remaining base_keys are deleted entries
    for key in base_keys.keys() {
        differences.push(serde_json::json!({
            "type": "deleted",
            "index": key.to_string(),
        }));
    }

    Ok(serde_json::json!({
        "base_ledger": base_seq,
        "diff_ledger": diff_seq,
        "differences": differences,
    }))
}
