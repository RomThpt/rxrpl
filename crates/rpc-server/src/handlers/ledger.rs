use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

fn ledger_header_json(ledger: &rxrpl_ledger::Ledger) -> Value {
    serde_json::json!({
        "ledger_index": ledger.header.sequence,
        "ledger_hash": ledger.header.hash.to_string(),
        "close_time": ledger.header.close_time,
        "parent_hash": ledger.header.parent_hash.to_string(),
        "total_coins": ledger.header.drops.to_string(),
        "closed": !ledger.is_open(),
    })
}

pub async fn ledger(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger_index = params
        .get("ledger_index")
        .and_then(|v| v.as_str())
        .unwrap_or("current");

    // In reporting mode, query the ledger store for historical headers
    if ctx.reporting_mode {
        if let Some(ref store) = ctx.ledger_store {
            let seq = match ledger_index {
                "current" | "closed" | "validated" => {
                    store
                        .latest_sequence()
                        .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                        .ok_or_else(|| {
                            RpcServerError::Internal("no ledger data available yet".into())
                        })?
                }
                index => index.parse::<u32>().map_err(|_| {
                    RpcServerError::InvalidParams(format!("invalid ledger_index: {index}"))
                })?,
            };

            let record = store
                .get_ledger_header(seq)
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                .ok_or_else(|| {
                    RpcServerError::InvalidParams(format!("ledger {seq} not found"))
                })?;

            return Ok(serde_json::json!({
                "ledger": {
                    "ledger_index": record.sequence,
                    "ledger_hash": hex::encode(&record.hash),
                    "closed": true,
                }
            }));
        }
        return Err(RpcServerError::Internal(
            "reporting mode has no ledger store".into(),
        ));
    }

    match ledger_index {
        "current" => {
            let ledger = ctx
                .ledger
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;
            let ledger = ledger.read().await;
            Ok(serde_json::json!({ "ledger": ledger_header_json(&ledger) }))
        }
        "closed" | "validated" => {
            let closed = ctx
                .closed_ledgers
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;
            let closed = closed.read().await;
            let ledger = closed
                .back()
                .ok_or_else(|| RpcServerError::Internal("no closed ledger yet".into()))?;
            Ok(serde_json::json!({ "ledger": ledger_header_json(ledger) }))
        }
        index => {
            let seq: u32 = index.parse().map_err(|_| {
                RpcServerError::InvalidParams(format!("invalid ledger_index: {index}"))
            })?;

            // Check current open ledger first
            if let Some(ref l) = ctx.ledger {
                let l = l.read().await;
                if l.header.sequence == seq {
                    return Ok(serde_json::json!({ "ledger": ledger_header_json(&l) }));
                }
            }

            // Search closed ledgers
            let closed = ctx
                .closed_ledgers
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no closed ledgers available".into()))?;
            let closed = closed.read().await;
            let ledger = closed
                .iter()
                .find(|l| l.header.sequence == seq)
                .ok_or_else(|| RpcServerError::InvalidParams(format!("ledger {seq} not found")))?;
            Ok(serde_json::json!({ "ledger": ledger_header_json(ledger) }))
        }
    }
}
