use std::str::FromStr;
use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use rxrpl_primitives::Hash256;

/// Look up the transaction history for a specific NFT.
///
/// Returns transactions that affected the given NFT (mint, transfers, burns,
/// offer operations), newest first, read from the NFT-transaction index
/// populated at ledger close.
pub async fn nft_history(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let nft_id_str = params
        .get("nft_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcServerError::InvalidParams("missing 'nft_id'".into()))?;

    let nft_id = Hash256::from_str(nft_id_str)
        .map_err(|e| RpcServerError::InvalidParams(format!("invalid nft_id: {e}")))?;

    let ledger_index_min = params
        .get("ledger_index_min")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let ledger_index_max = params
        .get("ledger_index_max")
        .and_then(|v| v.as_u64())
        .unwrap_or(u32::MAX as u64) as u32;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as u32;

    let store = ctx
        .tx_store
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no transaction store available".into()))?;

    let tx_hashes = store
        .get_nft_transactions(nft_id.as_bytes(), limit, ledger_index_min, ledger_index_max)
        .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?;

    let mut transactions = Vec::new();
    for tx_hash in &tx_hashes {
        if let Some(record) = store
            .get_transaction(tx_hash)
            .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
        {
            let tx_json: Value = serde_json::from_slice(&record.tx_blob).unwrap_or(Value::Null);
            let meta: Value = serde_json::from_slice(&record.meta_blob).unwrap_or(Value::Null);
            transactions.push(serde_json::json!({
                "tx": tx_json,
                "meta": meta,
                "ledger_index": record.ledger_seq,
                "validated": true,
            }));
        }
    }

    Ok(serde_json::json!({
        "nft_id": nft_id_str,
        "ledger_index_min": ledger_index_min,
        "ledger_index_max": ledger_index_max,
        "transactions": transactions,
        "validated": true,
    }))
}
