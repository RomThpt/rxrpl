use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Return the header of a specific ledger.
pub async fn ledger_header(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    Ok(serde_json::json!({
        "ledger": {
            "ledger_index": ledger.header.sequence,
            "ledger_hash": ledger.header.hash.to_string(),
            "parent_hash": ledger.header.parent_hash.to_string(),
            "total_coins": ledger.header.drops.to_string(),
            "close_time": ledger.header.close_time,
            "parent_close_time": ledger.header.parent_close_time,
            "close_time_resolution": ledger.header.close_time_resolution,
            "close_flags": ledger.header.close_flags,
            "account_hash": ledger.header.account_hash.to_string(),
            "transaction_hash": ledger.header.tx_hash.to_string(),
            "closed": !ledger.is_open(),
        },
        "ledger_index": ledger.header.sequence,
    }))
}
