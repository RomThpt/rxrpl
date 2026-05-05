use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers::common::resolve_ledger;

/// Operator-oriented ledger information.
///
/// Returns enriched ledger data with age in seconds (relative to the host
/// clock), transaction count, and parent hash diff. Mirrors the operator
/// summary fields from rippled's expanded `ledger`/`server_info` blocks.
pub async fn ledger_info(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let ledger = resolve_ledger(&params, ctx).await?;

    // Age = host_now (UNIX epoch) - close_time (Ripple epoch -> UNIX epoch).
    // For an open ledger, `close_time` is 0; report age 0 in that case
    // rather than a multi-decade value.
    let age_seconds: i64 = if ledger.header.close_time == 0 {
        0
    } else {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
            let close_unix =
                ledger.header.close_time as i64 + RIPPLE_EPOCH_OFFSET as i64;
            (now_unix - close_unix).max(0)
    };

    let transaction_count = ledger.tx_map.iter().count() as u64;

    Ok(serde_json::json!({
        "ledger": {
            "ledger_index": ledger.header.sequence,
            "ledger_hash": ledger.header.hash.to_string(),
            "parent_hash": ledger.header.parent_hash.to_string(),
            "total_coins": ledger.header.drops.to_string(),
            "close_time": ledger.header.close_time,
            "parent_close_time": ledger.header.parent_close_time,
            "age_seconds": age_seconds,
            "transaction_count": transaction_count,
            "account_hash": ledger.header.account_hash.to_string(),
            "transaction_hash": ledger.header.tx_hash.to_string(),
            "closed": !ledger.is_open(),
        },
        "ledger_index": ledger.header.sequence,
    }))
}
