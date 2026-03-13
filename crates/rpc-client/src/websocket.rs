use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{debug, warn};

use crate::error::ClientError;
use crate::subscription::SubscriptionStream;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Configuration for the WebSocket transport.
#[derive(Clone, Debug)]
pub struct WebSocketConfig {
    pub url: String,
    pub request_timeout: Duration,
    pub ping_interval: Duration,
    pub pong_timeout: Duration,
    pub auto_reconnect: bool,
    pub reconnect_delay_initial: Duration,
    pub reconnect_delay_max: Duration,
    pub reconnect_backoff_multiplier: f64,
    pub max_reconnect_attempts: Option<u32>,
    pub subscription_buffer_size: usize,
}

impl WebSocketConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            request_timeout: Duration::from_secs(30),
            ping_interval: Duration::from_secs(30),
            pong_timeout: Duration::from_secs(10),
            auto_reconnect: true,
            reconnect_delay_initial: Duration::from_secs(1),
            reconnect_delay_max: Duration::from_secs(30),
            reconnect_backoff_multiplier: 2.0,
            max_reconnect_attempts: None,
            subscription_buffer_size: 256,
        }
    }
}

/// Internal command sent from public API to the background actor.
enum Command {
    Request {
        id: u64,
        payload: String,
        response_tx: oneshot::Sender<Result<Value, ClientError>>,
    },
    Shutdown,
}

/// WebSocket transport using an actor pattern.
///
/// A background task owns the WebSocket connection and demultiplexes
/// incoming messages: responses are routed to the requesting caller via
/// oneshot channels, and subscription events are broadcast to all
/// `SubscriptionStream` receivers.
pub struct WebSocketTransport {
    command_tx: mpsc::Sender<Command>,
    id_counter: AtomicU64,
    subscription_tx: broadcast::Sender<Value>,
    task_handle: Mutex<Option<JoinHandle<()>>>,
    config: WebSocketConfig,
}

impl WebSocketTransport {
    /// Connect to the given WebSocket URL and spawn the background actor.
    pub async fn connect(config: WebSocketConfig) -> Result<Self, ClientError> {
        let (ws_stream, _) = connect_async(&config.url)
            .await
            .map_err(|e| ClientError::WebSocket(e.to_string()))?;

        let (command_tx, command_rx) = mpsc::channel::<Command>(64);
        let (subscription_tx, _) = broadcast::channel::<Value>(config.subscription_buffer_size);

        let sub_tx = subscription_tx.clone();
        let bg_config = config.clone();

        let task_handle = tokio::spawn(run_background_task(
            ws_stream,
            command_rx,
            sub_tx,
            bg_config,
        ));

        Ok(Self {
            command_tx,
            id_counter: AtomicU64::new(1),
            subscription_tx,
            task_handle: Mutex::new(Some(task_handle)),
            config,
        })
    }

    /// Send an RPC request and wait for the matching response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed);

        let mut cmd = serde_json::Map::new();
        cmd.insert("command".to_string(), Value::String(method.to_string()));
        cmd.insert("id".to_string(), Value::Number(id.into()));

        if let Some(obj) = params.as_object() {
            for (k, v) in obj {
                cmd.insert(k.clone(), v.clone());
            }
        }

        let payload = serde_json::to_string(&cmd).map_err(ClientError::from)?;

        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send(Command::Request {
                id,
                payload,
                response_tx,
            })
            .await
            .map_err(|_| ClientError::Connection("background task gone".to_string()))?;

        tokio::time::timeout(self.config.request_timeout, response_rx)
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|_| ClientError::Connection("response channel dropped".to_string()))?
    }

    /// Read the next subscription event (backward-compat wrapper).
    pub async fn next_message(&self) -> Result<Value, ClientError> {
        let mut rx = self.subscription_tx.subscribe();
        rx.recv()
            .await
            .map_err(|_| ClientError::SubscriptionClosed)
    }

    /// Get an independent subscription event stream.
    pub fn subscription_stream(&self) -> SubscriptionStream {
        SubscriptionStream::new(self.subscription_tx.subscribe())
    }

    /// Gracefully shut down the background task.
    pub async fn close(&self) -> Result<(), ClientError> {
        let _ = self.command_tx.send(Command::Shutdown).await;
        let mut handle = self.task_handle.lock().await;
        if let Some(h) = handle.take() {
            let _ = h.await;
        }
        Ok(())
    }
}

impl Drop for WebSocketTransport {
    fn drop(&mut self) {
        if let Some(handle) = self.task_handle.get_mut().take() {
            handle.abort();
        }
    }
}

/// The actor loop: owns the WS connection, routes messages, handles reconnection.
async fn run_background_task(
    ws_stream: WsStream,
    mut command_rx: mpsc::Receiver<Command>,
    subscription_tx: broadcast::Sender<Value>,
    config: WebSocketConfig,
) {
    let mut current_stream = Some(ws_stream);
    let mut reconnect_attempts: u32 = 0;

    loop {
        let ws = match current_stream.take() {
            Some(ws) => ws,
            None => {
                // Reconnect
                if !config.auto_reconnect {
                    debug!("auto_reconnect disabled, shutting down");
                    drain_pending(&mut command_rx, "connection closed (no reconnect)");
                    return;
                }

                if let Some(max) = config.max_reconnect_attempts {
                    if reconnect_attempts >= max {
                        warn!("max reconnect attempts ({max}) reached, shutting down");
                        drain_pending(&mut command_rx, "max reconnect attempts reached");
                        return;
                    }
                }

                let delay = compute_backoff_delay(
                    reconnect_attempts,
                    config.reconnect_delay_initial,
                    config.reconnect_delay_max,
                    config.reconnect_backoff_multiplier,
                );
                reconnect_attempts += 1;
                debug!(
                    "reconnecting in {:?} (attempt {reconnect_attempts})",
                    delay
                );
                tokio::time::sleep(delay).await;

                match connect_async(&config.url).await {
                    Ok((ws, _)) => {
                        debug!("reconnected successfully");
                        reconnect_attempts = 0;
                        ws
                    }
                    Err(e) => {
                        warn!("reconnect failed: {e}");
                        // Will loop and try again
                        continue;
                    }
                }
            }
        };

        let (mut ws_write, mut ws_read) = ws.split();
        let mut pending: HashMap<u64, oneshot::Sender<Result<Value, ClientError>>> =
            HashMap::new();
        let mut ping_interval = tokio::time::interval(config.ping_interval);
        ping_interval.reset(); // Don't fire immediately
        let mut awaiting_pong = false;
        let mut pong_deadline: Option<tokio::time::Instant> = None;

        loop {
            let pong_timeout = match pong_deadline {
                Some(deadline) => tokio::time::sleep_until(deadline),
                None => tokio::time::sleep(Duration::from_secs(86400)), // effectively never
            };

            tokio::select! {
                // Incoming command from public API
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(Command::Request { id, payload, response_tx }) => {
                            match ws_write.send(Message::Text(payload)).await {
                                Ok(()) => {
                                    pending.insert(id, response_tx);
                                }
                                Err(e) => {
                                    let _ = response_tx.send(Err(ClientError::WebSocket(e.to_string())));
                                    // Connection broken, break to reconnect
                                    fail_all_pending(&mut pending, "connection lost");
                                    break;
                                }
                            }
                        }
                        Some(Command::Shutdown) | None => {
                            // Graceful shutdown
                            let _ = ws_write.close().await;
                            fail_all_pending(&mut pending, "shutting down");
                            return;
                        }
                    }
                }

                // Incoming WS message
                msg = ws_read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<Value>(&text) {
                                Ok(value) => {
                                    route_message(value, &mut pending, &subscription_tx);
                                }
                                Err(e) => {
                                    warn!("failed to parse WS message: {e}");
                                }
                            }
                        }
                        Some(Ok(Message::Pong(_))) => {
                            awaiting_pong = false;
                            pong_deadline = None;
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = ws_write.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            debug!("connection closed");
                            fail_all_pending(&mut pending, "connection closed");
                            break; // will reconnect
                        }
                        Some(Err(e)) => {
                            warn!("WS error: {e}");
                            fail_all_pending(&mut pending, &format!("WS error: {e}"));
                            break; // will reconnect
                        }
                        _ => {}
                    }
                }

                // Ping timer
                _ = ping_interval.tick() => {
                    if awaiting_pong {
                        // Previous ping was never answered -- connection is dead
                        warn!("pong timeout, reconnecting");
                        fail_all_pending(&mut pending, "pong timeout");
                        break;
                    }
                    match ws_write.send(Message::Ping(vec![])).await {
                        Ok(()) => {
                            awaiting_pong = true;
                            pong_deadline = Some(tokio::time::Instant::now() + config.pong_timeout);
                        }
                        Err(e) => {
                            warn!("failed to send ping: {e}");
                            fail_all_pending(&mut pending, "failed to send ping");
                            break;
                        }
                    }
                }

                // Pong timeout
                _ = pong_timeout, if awaiting_pong => {
                    warn!("pong timeout, reconnecting");
                    fail_all_pending(&mut pending, "pong timeout");
                    break;
                }
            }
        }
        // Broke out of inner loop -- will reconnect at top of outer loop
        // current_stream is None, which triggers reconnection logic
    }
}

/// Route an incoming JSON message to the correct pending request or broadcast.
fn route_message(
    value: Value,
    pending: &mut HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>,
    subscription_tx: &broadcast::Sender<Value>,
) {
    // Messages with an `id` field are request responses
    if let Some(resp_id) = value.get("id").and_then(|v| v.as_u64()) {
        if let Some(tx) = pending.remove(&resp_id) {
            // Check for error
            if let Some(error) = value.get("error") {
                let error_str = error.as_str().unwrap_or("unknown").to_string();
                let error_code = value
                    .get("error_code")
                    .and_then(|c| c.as_i64())
                    .unwrap_or(-1) as i32;
                let error_message = value
                    .get("error_message")
                    .and_then(|m| m.as_str())
                    .map(String::from);
                let _ = tx.send(Err(ClientError::Rpc {
                    error: error_str,
                    code: error_code,
                    message: error_message,
                }));
            } else if let Some(result) = value.get("result") {
                let _ = tx.send(Ok(result.clone()));
            } else {
                let _ = tx.send(Ok(value));
            }
            return;
        }
    }

    // No matching id -- treat as subscription event
    let _ = subscription_tx.send(value);
}

/// Fail all pending requests with the given reason.
fn fail_all_pending(
    pending: &mut HashMap<u64, oneshot::Sender<Result<Value, ClientError>>>,
    reason: &str,
) {
    for (_, tx) in pending.drain() {
        let _ = tx.send(Err(ClientError::Connection(reason.to_string())));
    }
}

/// Drain all commands from the channel, failing any pending requests.
fn drain_pending(command_rx: &mut mpsc::Receiver<Command>, reason: &str) {
    while let Ok(cmd) = command_rx.try_recv() {
        if let Command::Request { response_tx, .. } = cmd {
            let _ = response_tx.send(Err(ClientError::Connection(reason.to_string())));
        }
    }
}

/// Compute exponential backoff delay.
fn compute_backoff_delay(
    attempt: u32,
    initial: Duration,
    max: Duration,
    multiplier: f64,
) -> Duration {
    let delay = initial.mul_f64(multiplier.powi(attempt as i32));
    delay.min(max)
}
