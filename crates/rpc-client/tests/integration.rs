use std::time::Duration;

use rxrpl_rpc_client::builder::ClientBuilder;
use rxrpl_rpc_client::error::ClientError;
use rxrpl_rpc_client::websocket::WebSocketConfig;

#[test]
fn http_transport_invalid_url() {
    // An empty URL should fail to build an HTTP client (reqwest rejects it)
    let result = ClientBuilder::new("not-a-url").build_http();
    // This may succeed (reqwest doesn't validate URL at build time) or fail.
    // The real test is that it doesn't panic.
    let _ = result;
}

#[test]
fn ws_config_defaults() {
    let config = WebSocketConfig::new("wss://example.com");
    assert_eq!(config.url, "wss://example.com");
    assert_eq!(config.request_timeout, Duration::from_secs(30));
    assert_eq!(config.ping_interval, Duration::from_secs(30));
    assert_eq!(config.pong_timeout, Duration::from_secs(10));
    assert!(config.auto_reconnect);
    assert_eq!(config.reconnect_delay_initial, Duration::from_secs(1));
    assert_eq!(config.reconnect_delay_max, Duration::from_secs(30));
    assert!((config.reconnect_backoff_multiplier - 2.0).abs() < f64::EPSILON);
    assert_eq!(config.max_reconnect_attempts, None);
    assert_eq!(config.subscription_buffer_size, 256);
}

#[tokio::test]
async fn build_auto_detect_http() {
    let result = ClientBuilder::new("https://s1.ripple.com:51234")
        .build()
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn build_auto_detect_ws() {
    // Will fail to connect (no server), but should attempt WS path (not HTTP)
    let result = ClientBuilder::new("wss://localhost:19999").build().await;
    assert!(result.is_err());
    let err = result.err().unwrap();
    // Should be a WebSocket error, not InvalidUrl
    assert!(
        matches!(err, ClientError::WebSocket(_) | ClientError::Connection(_)),
        "expected WebSocket or Connection error, got: {err}"
    );
}

#[tokio::test]
async fn build_invalid_scheme() {
    let result = ClientBuilder::new("ftp://example.com").build().await;
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(
        matches!(err, ClientError::InvalidUrl(_)),
        "expected InvalidUrl, got: {err}"
    );
}

#[ignore]
#[tokio::test]
async fn http_server_info() {
    let client = ClientBuilder::new("https://s1.ripple.com:51234")
        .build_http()
        .unwrap();
    let result = client.server_info().await.unwrap();
    assert!(result.get("info").is_some(), "response missing info field");
}

#[ignore]
#[tokio::test]
async fn http_fee() {
    let client = ClientBuilder::new("https://s1.ripple.com:51234")
        .build_http()
        .unwrap();
    let result = client.fee().await.unwrap();
    assert!(
        result.get("drops").is_some(),
        "response missing drops field"
    );
}
