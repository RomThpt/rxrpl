use std::sync::Arc;

use serde_json::Value;

use rxrpl_amendment::Rules;
use rxrpl_protocol::tx::compute_tx_hash;
use rxrpl_txq::{FeeLevel, FeeMetrics, QueueEntry};

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn submit(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    // In reporting mode, forward submit requests to the upstream node
    if ctx.reporting_mode {
        let forward_url = ctx
            .forward_url
            .as_ref()
            .ok_or_else(|| RpcServerError::Internal("no forward URL configured".into()))?;

        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "method": "submit",
            "params": [params]
        });

        let response = client
            .post(forward_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| RpcServerError::Internal(format!("forward request failed: {e}")))?;

        let result: Value = response.json().await.map_err(|e| {
            RpcServerError::Internal(format!("failed to parse forward response: {e}"))
        })?;

        // The upstream response wraps the result under "result"
        return Ok(result.get("result").cloned().unwrap_or(result));
    }

    // Rippled's submit method accepts two forms:
    //   1. { tx_blob: "..." }                      — signed-only submit
    //   2. { secret: "...", tx_json: { ... } }     — sign-and-submit
    // Delegate form (2) to sign_and_submit so existing tooling keeps working.
    if params.get("tx_blob").is_none()
        && params.get("secret").is_some()
        && params.get("tx_json").is_some()
    {
        return Box::pin(super::sign_and_submit(params, ctx)).await;
    }

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

    // --- Fee escalation gate ---
    // Compute the minimum fee required given current queue utilization.
    let fee_drops = tx_json
        .get("Fee")
        .and_then(|f| f.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);

    if let Some(ref tq) = ctx.tx_queue {
        let q = tq.read().await;
        let metrics = FeeMetrics::from_queue(q.len(), q.max_size());
        let required = metrics.escalated_fee_drops(fees.base_fee);

        if fee_drops < required {
            return Ok(serde_json::json!({
                "engine_result": "telInsufFeeP",
                "engine_result_code": -394,
                "engine_result_message": format!(
                    "Fee of {} drops is not enough. The current open ledger fee is {} drops.",
                    fee_drops, required
                ),
                "tx_json": tx_json,
            }));
        }

        // Per-account queue depth check
        let account = tx_json
            .get("Account")
            .and_then(|a| a.as_str())
            .unwrap_or("");
        if q.account_txs(account).len() >= rxrpl_txq::MAX_ACCOUNT_QUEUE_DEPTH {
            return Ok(serde_json::json!({
                "engine_result": "telCantQueue",
                "engine_result_code": -396,
                "engine_result_message": format!(
                    "Per-account queue limit ({}) reached for {}.",
                    rxrpl_txq::MAX_ACCOUNT_QUEUE_DEPTH, account
                ),
                "tx_json": tx_json,
            }));
        }
    }

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
                    preflight_passed: true,
                };
                let _ = tx_queue.write().await.submit(entry);
            }

            // Relay to P2P network
            if let Some(ref relay) = ctx.relay_tx {
                let _ = relay.send((tx_hash, tx_bytes));
            }
        }
    }

    // Inject canonical tx hash into the returned tx_json so clients can read
    // it without recomputing — matches rippled's submit response shape.
    let mut tx_json_with_hash = tx_json;
    if let Ok(tx_hash) = compute_tx_hash(&tx_json_with_hash) {
        if let Some(obj) = tx_json_with_hash.as_object_mut() {
            obj.insert(
                "hash".into(),
                serde_json::Value::String(tx_hash.to_string()),
            );
        }
    }

    Ok(serde_json::json!({
        "engine_result": result.to_string(),
        "engine_result_code": result.code(),
        "tx_json": tx_json_with_hash,
    }))
}
