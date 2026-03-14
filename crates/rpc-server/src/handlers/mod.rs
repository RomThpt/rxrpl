use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Handle the `ping` RPC method.
pub async fn ping(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({}))
}

/// Handle the `server_info` RPC method.
pub async fn server_info(
    _params: Value,
    _ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "info": {
            "build_version": env!("CARGO_PKG_VERSION"),
            "server_state": "full",
            "complete_ledgers": "empty",
        }
    }))
}

/// Handle the `server_state` RPC method.
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

/// Handle the `fee` RPC method.
pub async fn fee(_params: Value, _ctx: &Arc<ServerContext>) -> Result<Value, RpcServerError> {
    Ok(serde_json::json!({
        "current_ledger_size": "0",
        "current_queue_size": "0",
        "drops": {
            "base_fee": "10",
            "median_fee": "5000",
            "minimum_fee": "10",
            "open_ledger_fee": "10",
        },
        "expected_ledger_size": "1000",
        "ledger_current_index": 1,
        "levels": {
            "median_level": "128000",
            "minimum_level": "256",
            "open_ledger_level": "256",
            "reference_level": "256",
        },
        "max_queue_size": "2000",
    }))
}
