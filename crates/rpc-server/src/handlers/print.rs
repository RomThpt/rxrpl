use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn print(_params: Value, ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    let ledger_seq = if let Some(ref l) = ctx.ledger {
        l.read().await.header.sequence
    } else {
        0
    };

    let queue_size = if let Some(ref q) = ctx.tx_queue {
        q.read().await.len()
    } else {
        0
    };

    Ok(serde_json::json!({
        "ledger_sequence": ledger_seq,
        "tx_queue_size": queue_size,
    }))
}
