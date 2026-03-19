use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Dry-run a transaction against the current ledger without committing changes.
pub async fn simulate(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let tx_json = params
        .get("tx_json")
        .cloned()
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'tx_json'".into()))?;

    let engine = ctx
        .tx_engine
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no tx engine available".into()))?;

    let fees = ctx
        .fees
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no fee settings available".into()))?;

    // Clone the current ledger to use as a sandbox
    let ledger_lock = ctx
        .ledger
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;

    let base_ledger = ledger_lock.read().await;
    let mut sandbox_ledger = base_ledger.clone();
    drop(base_ledger);

    let rules = Rules::new();
    let result = engine
        .apply(&tx_json, &mut sandbox_ledger, &rules, fees)
        .map_err(|e| RpcServerError::Internal(format!("tx engine error: {e}")))?;

    Ok(serde_json::json!({
        "engine_result": result.to_string(),
        "engine_result_code": result.code(),
        "tx_json": tx_json,
        "applied": false,
    }))
}
