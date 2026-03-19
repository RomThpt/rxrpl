use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the configured UNL (Unique Node List) of trusted validators.
///
/// Admin-only method matching rippled's `unl_list` RPC. Currently returns an
/// empty list as the UNL is configured at the consensus layer and not directly
/// accessible from the RPC context.
pub async fn unl_list(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "node_size": 0,
        "validator_list": [],
    }))
}
