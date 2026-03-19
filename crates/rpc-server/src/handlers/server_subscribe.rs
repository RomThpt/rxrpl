use std::sync::Arc;

use serde_json::Value;

use crate::context::ServerContext;
use crate::error::RpcServerError;

/// Subscribe to server events via HTTP polling.
///
/// Unlike WebSocket-based `subscribe`, this returns a snapshot of the
/// current server state for the requested event streams. Supported
/// streams: `ledger`, `server`, `transactions`, `validations`.
pub async fn server_subscribe(
    params: Value,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let streams = params
        .get("streams")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut result = serde_json::Map::new();

    for stream in &streams {
        let stream_name = stream.as_str().unwrap_or_default();
        match stream_name {
            "ledger" => {
                if let Some(ref closed) = ctx.closed_ledgers {
                    let closed = closed.read().await;
                    if let Some(last) = closed.back() {
                        result.insert(
                            "ledger_index".into(),
                            Value::from(last.header.sequence),
                        );
                        result.insert(
                            "ledger_hash".into(),
                            Value::from(last.header.hash.to_string()),
                        );
                    }
                }
            }
            "server" => {
                result.insert(
                    "server_status".into(),
                    Value::from("full"),
                );
            }
            "transactions" | "transactions_proposed" => {
                // Acknowledged; events would be delivered via polling
                result.insert(
                    format!("{stream_name}_subscribed"),
                    Value::Bool(true),
                );
            }
            "validations" => {
                result.insert(
                    "validations_subscribed".into(),
                    Value::Bool(true),
                );
            }
            _ => {
                return Err(RpcServerError::InvalidParams(format!(
                    "unknown stream: {stream_name}"
                )));
            }
        }
    }

    Ok(Value::Object(result))
}
