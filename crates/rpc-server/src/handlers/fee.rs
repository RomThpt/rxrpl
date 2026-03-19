use std::sync::Arc;

use serde_json::Value;

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

    Ok(serde_json::json!({
        "current_ledger_size": "0",
        "current_queue_size": "0",
        "drops": {
            "base_fee": base_fee.to_string(),
            "median_fee": "5000",
            "minimum_fee": base_fee.to_string(),
            "open_ledger_fee": base_fee.to_string(),
        },
        "expected_ledger_size": "1000",
        "ledger_current_index": ledger_current_index,
        "levels": {
            "median_level": "128000",
            "minimum_level": "256",
            "open_ledger_level": "256",
            "reference_level": "256",
        },
        "max_queue_size": "2000",
    }))
}
