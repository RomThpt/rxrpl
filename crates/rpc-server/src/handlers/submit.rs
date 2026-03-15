use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn submit(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let tx_blob = params
        .get("tx_blob")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'tx_blob' field".into()))?;

    let tx_bytes = hex::decode(tx_blob)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid hex: {e}")))?;

    let tx_json = rxrpl_codec::binary::decode(&tx_bytes)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid tx blob: {e}")))?;

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
