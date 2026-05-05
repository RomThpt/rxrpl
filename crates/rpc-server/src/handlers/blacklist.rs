use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Deprecated admin command for peer blacklist management.
///
/// In rippled this was used to manage peer IP blacklists. It has been
/// superseded by the `peer_reservations_*` family of commands.
///
/// This implementation returns empty results with a deprecation notice.
pub async fn blacklist(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "deprecated": "Use peer_reservations_add/del/list instead.",
        "blacklist": [],
    }))
}
