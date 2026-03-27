use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers;

/// Convenience method that combines signing and submitting a transaction.
///
/// This is a shorthand for calling `sign` followed by `submit`. The
/// parameters are the same as `sign` -- the signed blob is automatically
/// submitted to the network.
pub async fn sign_and_submit(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    // First sign the transaction
    let sign_result = handlers::sign(params.clone(), ctx).await?;

    // Extract the signed blob
    let tx_blob = sign_result
        .get("tx_blob")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::Internal("sign did not return tx_blob".into()))?;

    // Submit the signed blob
    let submit_params = serde_json::json!({
        "tx_blob": tx_blob,
    });
    let submit_result = handlers::submit(submit_params, ctx).await?;

    // Merge both results
    let mut result = serde_json::Map::new();
    if let Value::Object(sign_map) = &sign_result {
        for (k, v) in sign_map {
            result.insert(k.clone(), v.clone());
        }
    }
    if let Value::Object(submit_map) = &submit_result {
        for (k, v) in submit_map {
            result.insert(k.clone(), v.clone());
        }
    }

    Ok(Value::Object(result))
}
