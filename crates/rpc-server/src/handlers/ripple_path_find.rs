use std::sync::Arc;

use serde_json::Value;

use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_pathfind::{PathRequest, parse_amount_issue, path_step_to_json};

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

pub async fn ripple_path_find(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    let source_str = params
        .get("source_account")
        .and_then(|v| v.as_str())
        .ok_or(RpcServerError::SourceAccountMissing)?;

    let destination_str = params
        .get("destination_account")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'destination_account'".into()))?;

    let destination_amount = params
        .get("destination_amount")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'destination_amount'".into()))?;

    let source = decode_account_id(source_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid source_account: {e}")))?;

    let destination = decode_account_id(destination_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid destination_account: {e}")))?;

    // Parse optional source_currencies
    let source_currencies =
        if let Some(arr) = params.get("source_currencies").and_then(|v| v.as_array()) {
            let mut issues = Vec::new();
            for item in arr {
                if let Some(issue) = parse_amount_issue(item) {
                    issues.push(issue);
                }
            }
            Some(issues)
        } else {
            None
        };

    let request = PathRequest {
        source,
        destination,
        destination_amount: destination_amount.clone(),
        source_currencies,
    };

    let alternatives = request.find_paths(&ledger);

    let alternatives_json: Vec<Value> = alternatives
        .iter()
        .map(|alt| {
            let paths: Vec<Value> = alt
                .paths_computed
                .iter()
                .map(|path| {
                    let steps: Vec<Value> = path.iter().map(path_step_to_json).collect();
                    Value::Array(steps)
                })
                .collect();

            serde_json::json!({
                "source_amount": alt.source_amount,
                "paths_computed": paths,
            })
        })
        .collect();

    Ok(serde_json::json!({
        "alternatives": alternatives_json,
        "destination_account": destination_str,
        "destination_amount": destination_amount,
        "source_account": source_str,
        "full_reply": true,
        "ledger_index": ledger.header.sequence,
    }))
}
