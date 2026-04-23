use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

pub async fn server_info(
    _params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let (complete_ledgers, server_state, ledger_index, validated_ledger) =
        if let Some(ref closed) = ctx.closed_ledgers {
            let closed = closed.read().await;
            if closed.is_empty() {
                ("empty".to_string(), "full", 1u32, None)
            } else {
                let first = closed.front().unwrap().header.sequence;
                let last_ledger = closed.back().unwrap();
                let last = last_ledger.header.sequence;
                let validated = serde_json::json!({
                    "seq": last,
                    "hash": last_ledger.header.hash.to_string(),
                    "close_time": last_ledger.header.close_time,
                    "base_fee_xrp": 0.00001,
                    "reserve_base_xrp": 10,
                    "reserve_inc_xrp": 2,
                });
                (format!("{first}-{last}"), "full", last, Some(validated))
            }
        } else {
            ("empty".to_string(), "full", 1, None)
        };

    let current_index = if let Some(ref ledger) = ctx.ledger {
        let l = ledger.read().await;
        l.header.sequence
    } else {
        ledger_index
    };

    let mut info = serde_json::json!({
        "build_version": env!("CARGO_PKG_VERSION"),
        "server_state": server_state,
        "complete_ledgers": complete_ledgers,
        "ledger_current_index": current_index,
    });
    if let Some(v) = validated_ledger {
        info["validated_ledger"] = v;
    }

    Ok(serde_json::json!({ "info": info }))
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
