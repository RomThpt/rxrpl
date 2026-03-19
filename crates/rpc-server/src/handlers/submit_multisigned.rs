use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn submit_multisigned(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let tx_json = params
        .get("tx_json")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'tx_json'".into()))?;

    // Verify it has Signers array and empty SigningPubKey
    if tx_json.get("Signers").and_then(|v| v.as_array()).is_none() {
        return Err(RpcServerError::InvalidParams(
            "tx_json must contain 'Signers' array".into(),
        ));
    }

    let signing_pub_key = tx_json
        .get("SigningPubKey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !signing_pub_key.is_empty() {
        return Err(RpcServerError::InvalidParams(
            "SigningPubKey must be empty for multisigned transactions".into(),
        ));
    }

    let ledger = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;

    let engine = ctx
        .tx_engine
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no tx engine available".into()))?;

    let fees = ctx
        .fees
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no fee settings available".into()))?;

    let mut ledger = ledger.write().await;
    let rules = Rules::new();

    let result = engine
        .apply(&tx_json, &mut ledger, &rules, fees)
        .map_err(|e| RpcServerError::Internal(format!("tx engine error: {e}")))?;

    Ok(serde_json::json!({
        "engine_result": result.to_string(),
        "engine_result_code": result.code(),
        "tx_json": tx_json,
    }))
}
