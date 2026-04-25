use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use rxrpl_primitives::Hash256;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

pub async fn transaction_entry(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let tx_hash_str = params
        .get("tx_hash")
        .and_then(|v| v.as_str())
        .ok_or(RpcServerError::FieldNotFoundTransaction)?;

    let tx_hash = Hash256::from_str(tx_hash_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid tx_hash: {e}")))?;

    // ledger_index is required for transaction_entry
    if params.get("ledger_index").is_none() {
        return Err(RpcServerError::InvalidParams(
            "missing 'ledger_index' (required for transaction_entry)".into(),
        ));
    }

    let ledger = resolve_ledger(&params, ctx).await?;

    let data = ledger.tx_map.get(&tx_hash).ok_or_else(|| {
        RpcServerError::InvalidParams("transaction not found in specified ledger".into())
    })?;

    let record: Value = serde_json::from_slice(data)
        .map_err(|e| RpcServerError::Internal(format!("failed to deserialize tx: {e}")))?;

    // If the stored data contains tx_json and metadata separately
    let tx_json = record.get("tx_json").unwrap_or(&record);
    let metadata = record.get("metadata").or_else(|| record.get("meta"));

    let mut result = serde_json::json!({
        "tx_json": tx_json,
        "ledger_index": ledger.header.sequence,
        "ledger_hash": ledger.header.hash.to_string(),
    });

    if let Some(meta) = metadata {
        result["metadata"] = meta.clone();
    }

    Ok(result)
}
