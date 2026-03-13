use std::time::Duration;

use crate::client::XrplClient;
use crate::error::ClientError;
use crate::http::HttpTransport;
use crate::transport::TransportKind;
use crate::websocket::{WebSocketConfig, WebSocketTransport};

/// Builder for constructing an XRPL client.
pub struct ClientBuilder {
    url: String,
    timeout: Option<Duration>,
    request_timeout: Option<Duration>,
    ping_interval: Option<Duration>,
    pong_timeout: Option<Duration>,
    auto_reconnect: Option<bool>,
    reconnect_delay_initial: Option<Duration>,
    reconnect_delay_max: Option<Duration>,
    reconnect_backoff_multiplier: Option<f64>,
    max_reconnect_attempts: Option<Option<u32>>,
    subscription_buffer_size: Option<usize>,
}

impl ClientBuilder {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            timeout: None,
            request_timeout: None,
            ping_interval: None,
            pong_timeout: None,
            auto_reconnect: None,
            reconnect_delay_initial: None,
            reconnect_delay_max: None,
            reconnect_backoff_multiplier: None,
            max_reconnect_attempts: None,
            subscription_buffer_size: None,
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    pub fn ping_interval(mut self, interval: Duration) -> Self {
        self.ping_interval = Some(interval);
        self
    }

    pub fn pong_timeout(mut self, timeout: Duration) -> Self {
        self.pong_timeout = Some(timeout);
        self
    }

    pub fn auto_reconnect(mut self, enabled: bool) -> Self {
        self.auto_reconnect = Some(enabled);
        self
    }

    pub fn reconnect_delay(mut self, initial: Duration, max: Duration) -> Self {
        self.reconnect_delay_initial = Some(initial);
        self.reconnect_delay_max = Some(max);
        self
    }

    pub fn reconnect_backoff_multiplier(mut self, multiplier: f64) -> Self {
        self.reconnect_backoff_multiplier = Some(multiplier);
        self
    }

    pub fn max_reconnect_attempts(mut self, max: Option<u32>) -> Self {
        self.max_reconnect_attempts = Some(max);
        self
    }

    pub fn subscription_buffer_size(mut self, size: usize) -> Self {
        self.subscription_buffer_size = Some(size);
        self
    }

    fn build_ws_config(&self) -> WebSocketConfig {
        let mut config = WebSocketConfig::new(&self.url);

        if let Some(t) = self.request_timeout.or(self.timeout) {
            config.request_timeout = t;
        }
        if let Some(i) = self.ping_interval {
            config.ping_interval = i;
        }
        if let Some(t) = self.pong_timeout {
            config.pong_timeout = t;
        }
        if let Some(r) = self.auto_reconnect {
            config.auto_reconnect = r;
        }
        if let Some(d) = self.reconnect_delay_initial {
            config.reconnect_delay_initial = d;
        }
        if let Some(d) = self.reconnect_delay_max {
            config.reconnect_delay_max = d;
        }
        if let Some(m) = self.reconnect_backoff_multiplier {
            config.reconnect_backoff_multiplier = m;
        }
        if let Some(m) = self.max_reconnect_attempts {
            config.max_reconnect_attempts = m;
        }
        if let Some(s) = self.subscription_buffer_size {
            config.subscription_buffer_size = s;
        }

        config
    }

    /// Build an HTTP-based client.
    pub fn build_http(self) -> Result<XrplClient, ClientError> {
        let transport = HttpTransport::new(&self.url)?;
        Ok(XrplClient::new(TransportKind::Http(transport)))
    }

    /// Build a WebSocket-based client (async).
    pub async fn build_ws(self) -> Result<XrplClient, ClientError> {
        let config = self.build_ws_config();
        let transport = WebSocketTransport::connect(config).await?;
        Ok(XrplClient::new(TransportKind::WebSocket(transport)))
    }

    /// Build a client, auto-detecting transport from URL scheme.
    pub async fn build(self) -> Result<XrplClient, ClientError> {
        if self.url.starts_with("ws://") || self.url.starts_with("wss://") {
            self.build_ws().await
        } else if self.url.starts_with("http://") || self.url.starts_with("https://") {
            self.build_http()
        } else {
            Err(ClientError::InvalidUrl(format!(
                "unsupported URL scheme: {}",
                self.url
            )))
        }
    }
}
