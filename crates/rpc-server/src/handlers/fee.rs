use std::sync::Arc;

use serde_json::Value;

use rxrpl_txq::{BASE_FEE_LEVEL, FeeMetrics};

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn fee(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let base_fee = ctx.fees.as_ref().map(|f| f.base_fee).unwrap_or(10);

    let (ledger_current_index, current_ledger_size) = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        (l.header.sequence, l.tx_map.iter().count())
    } else {
        (1, 0)
    };

    let (queue_size, max_queue_size) = if let Some(ref tq) = ctx.tx_queue {
        let q = tq.read().await;
        (q.len(), q.max_size())
    } else {
        (0, 2000)
    };

    let metrics = FeeMetrics::from_queue(queue_size, max_queue_size);
    let open_ledger_fee = metrics.escalated_fee_drops(base_fee);
    let open_ledger_level = metrics.escalated_fee_level(BASE_FEE_LEVEL);

    // median_fee/median_level would need the parent ledger's fee-level
    // distribution, which this node does not retain, so they fall back to the
    // base fee level; expected_ledger_size is rippled's fixed open-ledger target.
    Ok(serde_json::json!({
        "current_ledger_size": current_ledger_size.to_string(),
        "current_queue_size": queue_size.to_string(),
        "drops": {
            "base_fee": base_fee.to_string(),
            "median_fee": base_fee.to_string(),
            "minimum_fee": base_fee.to_string(),
            "open_ledger_fee": open_ledger_fee.to_string(),
        },
        "expected_ledger_size": "1000",
        "ledger_current_index": ledger_current_index,
        "levels": {
            "median_level": BASE_FEE_LEVEL.to_string(),
            "minimum_level": BASE_FEE_LEVEL.to_string(),
            "open_ledger_level": open_ledger_level.to_string(),
            "reference_level": BASE_FEE_LEVEL.to_string(),
        },
        "max_queue_size": max_queue_size.to_string(),
    }))
}
