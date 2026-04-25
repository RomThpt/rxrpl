use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Live UNL fetcher status. The overlay layer populates
/// `ctx.validator_list_status` with a JSON array of per-site status objects
/// each containing `site`, `last_fetch_unix`, `last_sequence`,
/// `last_validator_count`, and `last_error`.
pub async fn validator_list_sites(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let sites = match ctx.validator_list_status.as_ref() {
        Some(handle) => handle.read().await.clone(),
        None => Value::Array(Vec::new()),
    };
    Ok(serde_json::json!({
        "validator_sites": sites,
    }))
}
