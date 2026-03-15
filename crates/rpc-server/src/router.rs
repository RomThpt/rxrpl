use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::handlers;

/// Dispatch an RPC method call to the appropriate handler.
pub async fn dispatch(
    method: &str,
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    match method {
        "ping" => handlers::ping(params, ctx).await,
        "server_info" => handlers::server_info(params, ctx).await,
        "server_state" => handlers::server_state(params, ctx).await,
        "fee" => handlers::fee(params, ctx).await,
        "account_info" => handlers::account_info(params, ctx).await,
        "submit" => handlers::submit(params, ctx).await,
        "ledger" => handlers::ledger(params, ctx).await,
        "ledger_closed" => handlers::ledger_closed(params, ctx).await,
        "ledger_current" => handlers::ledger_current(params, ctx).await,
        "tx" => handlers::tx(params, ctx).await,
        _ => Err(RpcServerError::MethodNotFound(method.to_string())),
    }
}
