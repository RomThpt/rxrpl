use std::sync::Arc;

use rand::RngCore;
use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn random(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);

    Ok(serde_json::json!({
        "random": hex::encode_upper(bytes),
    }))
}
