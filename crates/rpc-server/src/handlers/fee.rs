use std::sync::Arc;

use serde_json::Value;

use rxrpl_txq::FeeMetrics;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn fee(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let base_fee = ctx.fees.as_ref().map(|f| f.base_fee).unwrap_or(10);

    let ledger_current_index = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        l.header.sequence
    } else {
        1
    };

    // Derive queue metrics from the live TxQueue.
    let (queue_size, max_queue_size) = if let Some(ref tq) = ctx.tx_queue {
        let q = tq.read().await;
        (q.len(), q.max_size())
    } else {
        (0, 2000)
    };

    let metrics = FeeMetrics::from_queue(queue_size, max_queue_size);
    let open_ledger_fee = metrics.escalated_fee_drops(base_fee);
    let open_ledger_level = metrics.escalated_fee_level(rxrpl_txq::BASE_FEE_LEVEL);

    Ok(serde_json::json!({
        "current_ledger_size": "0",
        "current_queue_size": queue_size.to_string(),
        "drops": {
            "base_fee": base_fee.to_string(),
            "median_fee": "5000",
            "minimum_fee": base_fee.to_string(),
            "open_ledger_fee": open_ledger_fee.to_string(),
        },
        "expected_ledger_size": "1000",
        "ledger_current_index": ledger_current_index,
        "levels": {
            "median_level": "128000",
            "minimum_level": "256",
            "open_ledger_level": open_ledger_level.to_string(),
            "reference_level": "256",
        },
        "max_queue_size": max_queue_size.to_string(),
    }))
}
