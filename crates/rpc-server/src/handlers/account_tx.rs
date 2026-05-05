use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::require_account_id;

pub async fn account_tx(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let account_id = require_account_id(&params)?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(200)
        .min(400) as u32;

    // In reporting mode, use the ledger store for account tx history
    if ctx.reporting_mode {
        if let Some(ref ledger_store) = ctx.ledger_store {
            let records = ledger_store
                .get_account_txs(account_id.as_bytes(), limit)
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?;

            let mut transactions = Vec::new();
            for record in &records {
                let tx_json: Value = serde_json::from_slice(&record.tx_blob).unwrap_or(Value::Null);
                let meta: Value = serde_json::from_slice(&record.meta_blob).unwrap_or(Value::Null);
                transactions.push(serde_json::json!({
                    "tx": tx_json,
                    "meta": meta,
                    "validated": true,
                }));
            }

            let account_str = params["account"].as_str().unwrap_or_default();
            return Ok(serde_json::json!({
                "account": account_str,
                "transactions": transactions,
            }));
        }
        return Err(RpcServerError::Internal(
            "reporting mode has no ledger store".into(),
        ));
    }

    let store = ctx
        .tx_store
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no transaction store available".into()))?;

    // Parse marker: "ledger_seq:tx_index"
    let (marker_seq, marker_idx) =
        if let Some(marker_str) = params.get("marker").and_then(|v| v.as_str()) {
            let parts: Vec<&str> = marker_str.split(':').collect();
            if parts.len() != 2 {
                return Err(RpcServerError::InvalidParams(
                    "invalid marker format, expected 'ledger_seq:tx_index'".into(),
                ));
            }
            let seq: u32 = parts[0]
                .parse()
                .map_err(|_| RpcServerError::InvalidParams("invalid marker ledger_seq".into()))?;
            let idx: u32 = parts[1]
                .parse()
                .map_err(|_| RpcServerError::InvalidParams("invalid marker tx_index".into()))?;
            (Some(seq), Some(idx))
        } else {
            (None, None)
        };

    // Fetch one extra to know if there's a next page
    let tx_hashes = store
        .get_account_transactions_with_marker(
            account_id.as_bytes(),
            limit + 1,
            marker_seq,
            marker_idx,
        )
        .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?;

    let has_more = tx_hashes.len() > limit as usize;
    let tx_hashes = if has_more {
        &tx_hashes[..limit as usize]
    } else {
        &tx_hashes[..]
    };

    let mut transactions = Vec::new();

    for tx_hash in tx_hashes {
        if let Some(record) = store
            .get_transaction(tx_hash)
            .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
        {
            let tx_json: Value = serde_json::from_slice(&record.tx_blob).unwrap_or(Value::Null);
            let meta: Value = serde_json::from_slice(&record.meta_blob).unwrap_or(Value::Null);

            transactions.push(serde_json::json!({
                "tx": tx_json,
                "meta": meta,
                "validated": true,
            }));
        }
    }

    let account_str = params["account"].as_str().unwrap_or_default();
    let mut result = serde_json::json!({
        "account": account_str,
        "transactions": transactions,
    });

    if has_more {
        if let Some(last_hash) = tx_hashes.last() {
            if let Some(record) = store.get_transaction(last_hash).ok().flatten() {
                result["marker"] =
                    Value::String(format!("{}:{}", record.ledger_seq, record.tx_index));
            }
        }
    }

    Ok(result)
}
