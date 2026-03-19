use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::router::dispatch;

pub async fn json(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let method = params
        .get("method")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'method'".into()))?;

    let inner_params = params
        .get("params")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    Box::pin(dispatch(method, inner_params, ctx)).await
}
