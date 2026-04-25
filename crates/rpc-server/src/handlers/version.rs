use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return supported XRPL JSON-RPC API versions.
///
/// rippled returns `{version: {first, last}}` where first/last are the
/// inclusive bounds of supported `api_version` values.
pub async fn version(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "version": {
            "first": 1,
            "last": 2,
        }
    }))
}
