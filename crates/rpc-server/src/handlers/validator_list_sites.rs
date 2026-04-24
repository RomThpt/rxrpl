use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn validator_list_sites(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "validator_sites": [],
    }))
}
