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

/// Selector parsed from the `ledger_index` param (string keyword or numeric seq).
enum LedgerSelector {
    Current,
    Closed,
    Validated,
    Sequence(u32),
}

fn parse_ledger_selector(params: &Value) -> Result<LedgerSelector, RpcServerError> {
    let raw = params.get("ledger_index").unwrap_or(&Value::Null);
    match raw {
        Value::Null => Ok(LedgerSelector::Current),
        Value::String(s) => match s.as_str() {
            "current" => Ok(LedgerSelector::Current),
            "closed" => Ok(LedgerSelector::Closed),
            "validated" => Ok(LedgerSelector::Validated),
            other => other
                .parse::<u32>()
                .map(LedgerSelector::Sequence)
                .map_err(|_| {
                    RpcServerError::InvalidParams(format!("invalid ledger_index: {other}"))
                }),
        },
        Value::Number(n) => n
            .as_u64()
            .and_then(|u| u32::try_from(u).ok())
            .map(LedgerSelector::Sequence)
            .ok_or_else(|| RpcServerError::InvalidParams(format!("invalid ledger_index: {n}"))),
        _ => Err(RpcServerError::InvalidParams(
            "invalid ledger_index type".into(),
        )),
    }
}

pub async fn ledger(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let selector = parse_ledger_selector(&params)?;

    // In reporting mode, query the ledger store for historical headers
    if ctx.reporting_mode {
        if let Some(ref store) = ctx.ledger_store {
            let seq = match selector {
                LedgerSelector::Current | LedgerSelector::Closed | LedgerSelector::Validated => {
                    store
                        .latest_sequence()
                        .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                        .ok_or_else(|| {
                            RpcServerError::Internal("no ledger data available yet".into())
                        })?
                }
                LedgerSelector::Sequence(s) => s,
            };

            let record = store
                .get_ledger_header(seq)
                .map_err(|e| RpcServerError::Internal(format!("storage error: {e}")))?
                .ok_or(RpcServerError::LedgerNotFound)?;

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

    match selector {
        LedgerSelector::Current => {
            let ledger = ctx
                .ledger
                .as_ref()
                .ok_or_else(|| RpcServerError::Internal("no ledger available".into()))?;
            let ledger = ledger.read().await;
            Ok(serde_json::json!({ "ledger": ledger_header_json(&ledger) }))
        }
        LedgerSelector::Closed | LedgerSelector::Validated => {
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
        LedgerSelector::Sequence(seq) => {
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
                .ok_or(RpcServerError::LedgerNotFound)?;
            Ok(serde_json::json!({ "ledger": ledger_header_json(ledger) }))
        }
    }
}
