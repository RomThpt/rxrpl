use std::sync::Arc;

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
use crate::events::event_to_json;
use crate::router::dispatch;
use crate::subscriptions::ConnectionSubscriptions;

/// Build the axum Router for the RPC server.
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
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_connection(socket, ctx))
}

/// Handle a single WebSocket connection.
async fn handle_ws_connection(socket: WebSocket, ctx: Arc<ServerContext>) {
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
                            "error": format!("invalid JSON: {e}"),
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
                    _ => {
                        // Standard RPC dispatch -- params come directly in
                        // the body for WS (no wrapping array like HTTP).
                        let result = dispatch(&method, body.clone(), &ctx).await;
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
                        if subscriptions.matches(&ev) {
                            let json = event_to_json(&ev);
                            if ws_tx.send(Message::Text(json.to_string().into())).await.is_err() {
                                break;
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
                "error": e.to_string(),
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
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let method = body.get("method").and_then(|v| v.as_str()).unwrap_or("");

    let params = body
        .get("params")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    match dispatch(method, params, &ctx).await {
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
                    "error": e.to_string(),
                }
            });
            (StatusCode::OK, Json(response))
        }
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
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
