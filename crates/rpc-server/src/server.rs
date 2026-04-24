use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::context::ServerContext;
use crate::error::RpcServerError;
use crate::events::{ServerEvent, event_to_json};
use crate::handlers::{build_path_find_response, parse_path_find_params, run_path_find};
use crate::role::{ConnectionRole, RequestContext};
use crate::router::dispatch;
use crate::subscriptions::ConnectionSubscriptions;

/// Build the axum Router for the RPC server.
///
/// Callers must use `into_make_service_with_connect_info::<SocketAddr>()`
/// when serving so that `ConnectInfo<SocketAddr>` is available.
pub fn build_router(ctx: Arc<ServerContext>) -> Router {
    Router::new()
        .route("/", post(rpc_handler).get(ws_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(ctx)
}

/// Serve Prometheus metrics in text exposition format.
async fn metrics_handler(State(ctx): State<Arc<ServerContext>>) -> impl IntoResponse {
    match &ctx.metrics_handle {
        Some(handle) => (StatusCode::OK, handle.render()),
        None => (StatusCode::NOT_FOUND, "metrics not enabled".to_string()),
    }
}

/// Handle WebSocket upgrade requests.
async fn ws_handler(
    State(ctx): State<Arc<ServerContext>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let role = ConnectionRole::from_ip(addr.ip(), &ctx.config);
    ws.on_upgrade(move |socket| handle_ws_connection(socket, ctx, role))
}

/// Handle a single WebSocket connection.
async fn handle_ws_connection(socket: WebSocket, ctx: Arc<ServerContext>, role: ConnectionRole) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut subscriptions = ConnectionSubscriptions::new();
    let mut event_rx = ctx.event_sender().subscribe();

    loop {
        tokio::select! {
            msg = ws_rx.next() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    _ => break, // disconnected or error
                };

                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => break,
                    _ => continue,
                };

                let body: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = serde_json::json!({
                            "result": {
                                "error": "invalidJson",
                                "error_code": 31,
                                "error_message": format!("invalid JSON: {e}"),
                                "status": "error",
                            },
                            "status": "error",
                            "type": "response",
                        });
                        let _ = ws_tx.send(Message::Text(err.to_string().into())).await;
                        continue;
                    }
                };

                let id = body.get("id").cloned();
                let method = body
                    .get("command")
                    .or_else(|| body.get("method"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let api_version = parse_api_version(&body);
                let req_ctx = RequestContext { role, api_version };

                let response = match method.as_str() {
                    "subscribe" => {
                        match subscriptions.apply_subscribe(&body) {
                            Ok(result) => ws_response(id, Ok(result)),
                            Err(e) => ws_response(id, Err(e)),
                        }
                    }
                    "unsubscribe" => {
                        match subscriptions.apply_unsubscribe(&body) {
                            Ok(result) => ws_response(id, Ok(result)),
                            Err(e) => ws_response(id, Err(e)),
                        }
                    }
                    "path_find" => {
                        let result = handle_ws_path_find(
                            &body,
                            &mut subscriptions,
                            &ctx,
                        ).await;
                        ws_response(id, result)
                    }
                    _ => {
                        // Standard RPC dispatch -- params come directly in
                        // the body for WS (no wrapping array like HTTP).
                        let result = dispatch(&method, body.clone(), &ctx, &req_ctx).await;
                        ws_response(id, result)
                    }
                };

                if ws_tx.send(Message::Text(response.to_string().into())).await.is_err() {
                    break;
                }
            }

            event = event_rx.recv() => {
                match event {
                    Ok(ev) => {
                        // Standard subscription-based event dispatch.
                        if subscriptions.matches(&ev) {
                            let json = event_to_json(&ev);
                            if ws_tx.send(Message::Text(json.to_string().into())).await.is_err() {
                                break;
                            }
                        }

                        // Re-run active path_find subscription on new validated ledger.
                        if let ServerEvent::LedgerClosed { .. } = &ev {
                            if subscriptions.path_find_subscription().is_some() {
                                if let Some(update) = rerun_path_find(&mut subscriptions, &ctx).await {
                                    if ws_tx.send(Message::Text(update.to_string().into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket consumer lagged, skipped {n} events");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// Handle `path_find` subcommands over WebSocket with per-connection state.
async fn handle_ws_path_find(
    params: &Value,
    subscriptions: &mut ConnectionSubscriptions,
    ctx: &Arc<ServerContext>,
) -> Result<Value, RpcServerError> {
    let subcommand = params
        .get("subcommand")
        .and_then(|v| v.as_str())
        .unwrap_or("create");

    match subcommand {
        "create" => {
            // Close any existing subscription first (rippled behavior).
            subscriptions.close_path_find();

            let sub = parse_path_find_params(params)?;

            // Run initial pathfinding against the current ledger.
            let ledger_ref = crate::handlers::common::resolve_ledger(params, ctx).await?;
            let (alts_json, serialized) = run_path_find(&sub, &ledger_ref);
            let response = build_path_find_response(&sub, &alts_json);

            // Store subscription with initial result for dedup.
            let mut sub = sub;
            sub.last_result = Some(serialized);
            subscriptions.create_path_find(sub)?;

            Ok(response)
        }
        "close" => {
            let was_active = subscriptions.close_path_find();
            if was_active {
                Ok(serde_json::json!({ "closed": true }))
            } else {
                Ok(serde_json::json!({ "closed": false, "status": "no path_find in progress" }))
            }
        }
        "status" => {
            if let Some(sub) = subscriptions.path_find_subscription() {
                // Re-run pathfinding to get current best paths.
                let ledger_ref = crate::handlers::common::resolve_ledger(params, ctx).await?;
                let (alts_json, _) = run_path_find(sub, &ledger_ref);
                Ok(build_path_find_response(sub, &alts_json))
            } else {
                Ok(serde_json::json!({ "status": "no path_find in progress" }))
            }
        }
        _ => Err(RpcServerError::InvalidParams(format!(
            "unknown subcommand: {subcommand}"
        ))),
    }
}

/// Re-run pathfinding for the active subscription on a new validated ledger.
///
/// Returns `Some(json)` if the result changed and an update should be sent,
/// or `None` if the result is unchanged (suppressed duplicate).
async fn rerun_path_find(
    subscriptions: &mut ConnectionSubscriptions,
    ctx: &Arc<ServerContext>,
) -> Option<Value> {
    // Get the current validated ledger.
    let closed = ctx.closed_ledgers.as_ref()?;
    let closed_guard = closed.read().await;
    let ledger = closed_guard.back()?;

    let sub = subscriptions.path_find_subscription()?;
    let (alts_json, serialized) = run_path_find(sub, ledger);

    // Compare with last result to suppress duplicates.
    if sub.last_result.as_deref() == Some(&serialized) {
        return None;
    }

    let update = serde_json::json!({
        "type": "path_find",
        "source_account": sub.source_account_str,
        "destination_account": sub.destination_account_str,
        "destination_amount": sub.destination_amount,
        "full_reply": true,
        "alternatives": alts_json,
    });

    // Update stored result for future dedup.
    if let Some(sub_mut) = subscriptions.path_find_subscription_mut() {
        sub_mut.last_result = Some(serialized);
    }

    Some(update)
}

/// Format a WebSocket RPC response with optional id.
fn ws_response(id: Option<Value>, result: Result<Value, crate::error::RpcServerError>) -> Value {
    match result {
        Ok(value) => {
            let mut obj = value.as_object().cloned().unwrap_or_default();
            obj.insert("status".into(), Value::String("success".into()));
            let mut resp = serde_json::json!({
                "result": obj,
                "status": "success",
                "type": "response",
            });
            if let Some(id) = id {
                resp["id"] = id;
            }
            resp
        }
        Err(e) => {
            let mut resp = serde_json::json!({
                "result": {
                    "error": e.token(),
                    "error_code": e.numeric_code(),
                    "error_message": e.human_message(),
                    "status": "error",
                },
                "status": "error",
                "type": "response",
            });
            if let Some(id) = id {
                resp["id"] = id;
            }
            resp
        }
    }
}

/// Handle JSON-RPC requests over HTTP.
async fn rpc_handler(
    State(ctx): State<Arc<ServerContext>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");

    let params = body
        .get("params")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let api_version = parse_api_version(&params);
    let role = ConnectionRole::from_ip(addr.ip(), &ctx.config);
    let req_ctx = RequestContext { role, api_version };

    match dispatch(method, params, &ctx, &req_ctx).await {
        Ok(result) => {
            let mut obj = result.as_object().cloned().unwrap_or_default();
            obj.insert("status".into(), Value::String("success".into()));
            let response = serde_json::json!({ "result": obj });
            (StatusCode::OK, Json(response))
        }
        Err(e) => {
            let response = serde_json::json!({
                "result": {
                    "status": "error",
                    "error": e.token(),
                    "error_code": e.numeric_code(),
                    "error_message": e.human_message(),
                    "request": body,
                }
            });
            (StatusCode::OK, Json(response))
        }
    }
}

/// Parse `api_version` from a JSON value, defaulting to V1.
fn parse_api_version(value: &Value) -> rxrpl_rpc_api::ApiVersion {
    match value.get("api_version").and_then(|v| v.as_u64()) {
        Some(2) => rxrpl_rpc_api::ApiVersion::V2,
        _ => rxrpl_rpc_api::ApiVersion::V1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use rxrpl_config::ServerConfig;
    use tower::ServiceExt;

    fn test_ctx() -> Arc<ServerContext> {
        ServerContext::new(ServerConfig::default())
    }

    #[tokio::test]
    async fn ping_request() {
        let app = build_router(test_ctx());
        let body = serde_json::json!({"method": "ping"});
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_method() {
        let app = build_router(test_ctx());
        let body = serde_json::json!({"method": "nonexistent"});
        let request = Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/json")
            .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))))
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
