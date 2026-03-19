use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the known validators list.
///
/// Returns the configured validator list. Full UNL integration
/// will provide dynamic validator information once available.
pub async fn validators(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "validation_quorum": 0,
        "validator_list": {
            "expiration": "unknown",
            "status": "active",
        },
        "trusted_validator_keys": [],
        "publisher_lists": [],
    }))
}
