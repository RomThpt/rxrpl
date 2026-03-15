use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn server_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let (complete_ledgers, server_state, ledger_index) =
        if let Some(ref closed) = ctx.closed_ledgers {
            let closed = closed.read().await;
            if closed.is_empty() {
                ("empty".to_string(), "full", 1u32)
            } else {
                let first = closed.front().unwrap().header.sequence;
                let last = closed.back().unwrap().header.sequence;
                (format!("{first}-{last}"), "full", last)
            }
        } else {
            ("empty".to_string(), "full", 1)
        };

    let current_index = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        l.header.sequence
    } else {
        ledger_index
    };

    Ok(serde_json::json!({
        "info": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": server_state,
            "complete_ledgers": complete_ledgers,
            "ledger_current_index": current_index,
        }
    }))
}

pub async fn server_state(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "state": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": "full",
        }
    }))
}
