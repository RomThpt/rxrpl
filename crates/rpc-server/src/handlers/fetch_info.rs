use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Admin command to return information about recent ledger fetches.
pub async fn fetch_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let (current_seq, closed_count) = if let Some(ref cl) = ctx.closed_ledgers {
        let history = cl.read().await;
        let seq = history.back().map(|l| l.header.sequence).unwrap_or(0);
        (seq, history.len())
    } else {
        (0, 0)
    };

    Ok(serde_json::json!({
        "info": {
            "ledger_seq": current_seq,
            "closed_ledgers": closed_count,
            "fetching": false,
        },
    }))
}
