use std::sync::Arc;

use serde_json::Value;

use rxrpl_txq::{BASE_FEE_LEVEL, FeeMetrics};

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Return queue state and per-account transaction details.
///
/// Optional params:
///   - `account`: restrict output to a single account.
pub async fn queue_info(params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let tq = ctx
        .tx_queue
        .as_ref()
        .ok_or_else(|| RpcServerError::Internal("no transaction queue available".into()))?;

    let q = tq.read().await;

    let queue_size = q.len();
    let max_size = q.max_size();

    let metrics = FeeMetrics::from_queue(queue_size, max_size);
    let base_level = BASE_FEE_LEVEL;
    let escalated_level = metrics.escalated_fee_level(base_level);

    let filter_account = params
        .get("account")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Build per-account transaction lists.
    let mut accounts_json: Vec<Value> = Vec::new();

    let account_keys: Vec<String> = if let Some(ref acct) = filter_account {
        if q.account_txs(acct).is_empty() {
            Vec::new()
        } else {
            vec![acct.clone()]
        }
    } else {
        q.accounts().cloned().collect()
    };

    for account in &account_keys {
        let hashes = q.account_txs(account);
        let mut txs_json: Vec<Value> = Vec::new();

        for hash in hashes {
            if let Some(entry) = q.get(hash) {
                txs_json.push(serde_json::json!({
                    "hash": format!("{}", hash),
                    "fee_level": entry.fee_level.value(),
                    "sequence": entry.sequence,
                    "last_ledger_sequence": entry.last_ledger_sequence,
                    "preflight_passed": entry.preflight_passed,
                }));
            }
        }

        accounts_json.push(serde_json::json!({
            "account": account,
            "queued_count": txs_json.len(),
            "transactions": txs_json,
        }));
    }

    let queue_metrics = &q.metrics;

    Ok(serde_json::json!({
        "queue_size": queue_size,
        "max_size": max_size,
        "fee_escalation": {
            "base_level": base_level,
            "escalated_level": escalated_level,
        },
        "accounts": accounts_json,
        "metrics": {
            "total_queued": queue_metrics.total_queued,
            "total_applied": queue_metrics.total_applied,
            "total_expired": queue_metrics.total_expired,
            "total_dropped": queue_metrics.total_dropped,
            "total_replaced": queue_metrics.total_replaced,
        },
    }))
}
