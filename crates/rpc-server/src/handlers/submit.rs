use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;
use rxrpl_protocol::tx::compute_tx_hash;
use rxrpl_txq::{FeeLevel, QueueEntry};

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

    // Emit proposed transaction event for WebSocket subscribers
    let _ = ctx
        .event_sender()
        .send(crate::events::ServerEvent::TransactionProposed {
            transaction: tx_json.clone(),
            engine_result: result.to_string(),
            engine_result_code: result.code(),
        });

    // On success: queue + relay
    if result.is_success() {
        // Compute tx hash
        if let Ok(tx_hash) = compute_tx_hash(&tx_json) {
            let account = tx_json
                .get("Account")
                .and_then(|a| a.as_str())
                .unwrap_or("")
                .to_string();
            let sequence = tx_json
                .get("Sequence")
                .and_then(|s| s.as_u64())
                .unwrap_or(0) as u32;
            let fee_drops = tx_json
                .get("Fee")
                .and_then(|f| f.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let last_ledger_sequence = tx_json
                .get("LastLedgerSequence")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32);

            // Queue
            if let Some(ref tx_queue) = ctx.tx_queue {
                let entry = QueueEntry {
                    hash: tx_hash,
                    tx: tx_json.clone(),
                    fee_level: FeeLevel::new(fee_drops, fees.base_fee),
                    account,
                    sequence,
                    last_ledger_sequence,
                };
                let _ = tx_queue.write().await.submit(entry);
            }

            // Relay to P2P network
            if let Some(ref relay) = ctx.relay_tx {
                let _ = relay.send((tx_hash, tx_bytes));
            }
        }
    }

    Ok(serde_json::json!({
        "engine_result": result.to_string(),
        "engine_result_code": result.code(),
        "tx_json": tx_json,
    }))
}
