use serde_json::Value;

use crate::error::ClientError;
use crate::subscription::SubscriptionStream;

/// Transport type enum (avoids async_trait dependency).
pub enum TransportKind {
    Http(crate::http::HttpTransport),
    WebSocket(crate::websocket::WebSocketTransport),
}

impl TransportKind {
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        match self {
            Self::Http(t) => t.request(method, params).await,
            Self::WebSocket(t) => t.request(method, params).await,
        }
    }

    /// Get an independent subscription event stream.
    /// Returns `None` for HTTP transport (subscriptions are WS-only).
    pub fn subscription_stream(&self) -> Option<SubscriptionStream> {
        match self {
            Self::Http(_) => None,
            Self::WebSocket(t) => Some(t.subscription_stream()),
        }
    }

    /// Read the next subscription message (backward compat).
    /// Returns error for HTTP transport.
    pub async fn next_message(&self) -> Result<Value, ClientError> {
        match self {
            Self::Http(_) => Err(ClientError::Other(
                "subscriptions not supported on HTTP transport".to_string(),
            )),
            Self::WebSocket(t) => t.next_message().await,
        }
    }

    /// Gracefully close the transport connection.
    pub async fn close(&self) -> Result<(), ClientError> {
        match self {
            Self::Http(_) => Ok(()),
            Self::WebSocket(t) => t.close().await,
        }
    }
}
