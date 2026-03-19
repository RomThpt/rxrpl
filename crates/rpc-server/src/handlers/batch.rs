use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::router::dispatch;

const MAX_BATCH_SIZE: usize = 50;

pub async fn batch(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let requests = params
        .get("requests")
        .and_then(|v| v.as_array())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'requests' array".into()))?;

    if requests.len() > MAX_BATCH_SIZE {
        return Err(RpcServerError::InvalidParams(format!(
            "batch exceeds maximum size of {MAX_BATCH_SIZE}"
        )));
    }

    let mut results = Vec::with_capacity(requests.len());
    for req in requests {
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

        if method == "batch" {
            results.push(serde_json::json!({
                "error": "batch-in-batch not allowed",
                "status": "error",
            }));
            continue;
        }

        let req_params = req
            .get("params")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));

        match Box::pin(dispatch(method, req_params, ctx)).await {
            Ok(result) => results.push(result),
            Err(e) => results.push(serde_json::json!({
                "error": e.to_string(),
                "status": "error",
            })),
        }
    }

    Ok(serde_json::json!({ "results": results }))
}
