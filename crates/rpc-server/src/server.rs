use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::Value;

use crate::context::ServerContext;
use crate::router::dispatch;

/// Build the axum Router for the RPC server.
pub fn build_router(ctx: Arc<ServerContext>) -> Router {
    Router::new()
        .route("/", post(rpc_handler))
        .with_state(ctx)
}

/// Handle JSON-RPC requests.
async fn rpc_handler(
    State(ctx): State<Arc<ServerContext>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let method = body
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let params = body
        .get("params")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    match dispatch(method, params, &ctx).await {
        Ok(result) => {
            let mut obj = result
                .as_object()
                .cloned()
                .unwrap_or_default();
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
