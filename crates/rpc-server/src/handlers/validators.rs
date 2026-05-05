use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return the known validators list.
///
/// Returns the configured validator list. Full UNL integration
/// will provide dynamic validator information once available.
pub async fn validators(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let validator_domains = match ctx.domain_attestation_status.as_ref() {
        Some(handle) => handle
            .read()
            .await
            .get("validators")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        None => Value::Array(Vec::new()),
    };
    Ok(serde_json::json!({
        "validation_quorum": 0,
        "validator_list": {
            "expiration": "unknown",
            "status": "active",
        },
        "trusted_validator_keys": [],
        "publisher_lists": [],
        "validator_domains": validator_domains,
    }))
}
