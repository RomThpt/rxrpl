use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use rxrpl_rpc_server::ServerContext;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = SplitSink<WsStream, Message>;
type WsReader = SplitStream<WsStream>;
use rxrpl_storage::LedgerStore;
use tokio::sync::RwLock;

use crate::error::NodeError;

/// Maximum backoff delay between reconnection attempts.
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);
/// Initial backoff delay between reconnection attempts.
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
/// Backoff multiplier for reconnection attempts.
const RECONNECT_BACKOFF_MULTIPLIER: f64 = 2.0;

/// ETL pipeline that receives validated ledgers from an upstream source.
///
/// Connects via WebSocket to a validating node and stores each validated
/// ledger into the local `LedgerStore` for serving via the reporting RPC.
pub struct EtlPipeline {
    etl_source: String,
    ledger_store: Arc<dyn LedgerStore>,
    /// Sequence of the last successfully stored ledger.
    last_seq: RwLock<Option<u32>>,
}

impl EtlPipeline {
    /// Create a new ETL pipeline targeting the given upstream WebSocket URL.
    pub fn new(etl_source: String, ledger_store: Arc<dyn LedgerStore>) -> Self {
        Self {
            etl_source,
            ledger_store,
            last_seq: RwLock::new(None),
        }
    }

    /// Receive and store a validated ledger from the upstream source.
    ///
    /// Stores the ledger header and all transactions, then indexes each
    /// transaction against its affected accounts for later retrieval.
    pub async fn receive_ledger(
        &self,
        seq: u32,
        hash: &[u8],
        header_blob: &[u8],
        txs: &[(Vec<u8>, Vec<u8>, Vec<u8>)], // (tx_hash, tx_blob, meta_blob)
    ) -> Result<(), NodeError> {
        self.ledger_store.store_ledger(seq, hash, header_blob)?;

        for (tx_hash, tx_blob, meta_blob) in txs {
            self.ledger_store
                .store_tx(tx_hash, seq, tx_blob, meta_blob)?;

            // Index account transactions by extracting affected accounts
            // from the transaction blob (Account field) and metadata
            // (AffectedNodes).
            let affected = extract_affected_accounts(tx_blob, meta_blob);
            for account in &affected {
                self.ledger_store.index_account_tx(account, tx_hash)?;
            }
        }

        *self.last_seq.write().await = Some(seq);
        tracing::info!(seq, "stored validated ledger from ETL source");
        Ok(())
    }

    /// Return the ETL source URL.
    pub fn source_url(&self) -> &str {
        &self.etl_source
    }

    /// Return the last stored ledger sequence.
    pub async fn last_stored_seq(&self) -> Option<u32> {
        *self.last_seq.read().await
    }

    /// Run the ETL extraction loop.
    ///
    /// Connects to the upstream WebSocket source, subscribes to the "ledger"
    /// stream, and fetches full ledger data for each validated ledger. On
    /// disconnect, reconnects with exponential backoff and resumes from the
    /// last stored sequence.
    pub async fn run(&self) -> Result<(), NodeError> {
        tracing::info!(source = %self.etl_source, "ETL pipeline starting");

        let mut reconnect_attempts: u32 = 0;

        loop {
            match self.run_connection().await {
                Ok(()) => {
                    // Clean exit (should not happen in normal operation)
                    tracing::info!("ETL connection closed cleanly");
                    return Ok(());
                }
                Err(e) => {
                    let delay = compute_backoff_delay(reconnect_attempts);
                    reconnect_attempts += 1;
                    tracing::warn!(
                        source = %self.etl_source,
                        error = %e,
                        attempt = reconnect_attempts,
                        delay_secs = delay.as_secs(),
                        "ETL connection lost, reconnecting"
                    );
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Establish a single WebSocket connection and process ledger events
    /// until the connection drops.
    async fn run_connection(&self) -> Result<(), NodeError> {
        let (ws_stream, _) = connect_async(&self.etl_source)
            .await
            .map_err(|e| NodeError::Server(format!("WebSocket connect failed: {e}")))?;

        tracing::info!(source = %self.etl_source, "connected to ETL source");

        let (mut ws_write, mut ws_read) = ws_stream.split();

        // Subscribe to the ledger stream
        let subscribe_msg = serde_json::json!({
            "command": "subscribe",
            "streams": ["ledger"]
        });
        ws_write
            .send(Message::Text(subscribe_msg.to_string()))
            .await
            .map_err(|e| NodeError::Server(format!("failed to send subscribe: {e}")))?;

        // Process incoming messages
        while let Some(msg) = ws_read.next().await {
            let msg = msg.map_err(|e| NodeError::Server(format!("WebSocket error: {e}")))?;

            let text = match msg {
                Message::Text(t) => t,
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(NodeError::Server("connection closed by peer".into()));
                }
                _ => continue,
            };

            let value: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("failed to parse ETL message: {e}");
                    continue;
                }
            };

            // Check if this is a ledger_closed event (subscription notification)
            if value.get("type").and_then(|v| v.as_str()) == Some("ledgerClosed") {
                if let Err(e) = self.handle_ledger_closed(&value, &mut ws_write, &mut ws_read).await {
                    tracing::warn!(error = %e, "failed to process ledger event");
                }
            }
        }

        Err(NodeError::Server("WebSocket stream ended".into()))
    }

    /// Handle a ledgerClosed subscription event by fetching the full ledger
    /// and storing it.
    async fn handle_ledger_closed(
        &self,
        event: &Value,
        ws_write: &mut WsWriter,
        ws_read: &mut WsReader,
    ) -> Result<(), NodeError> {
        let ledger_index = event
            .get("ledger_index")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| NodeError::Server("missing ledger_index in event".into()))?
            as u32;

        let ledger_hash = event
            .get("ledger_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Skip if we already have this ledger
        if let Some(last) = self.last_stored_seq().await {
            if ledger_index <= last {
                tracing::debug!(seq = ledger_index, "skipping already-stored ledger");
                return Ok(());
            }
        }

        tracing::debug!(seq = ledger_index, hash = ledger_hash, "fetching full ledger");

        // Request full ledger with expanded transactions
        let ledger_req = serde_json::json!({
            "command": "ledger",
            "ledger_index": ledger_index,
            "transactions": true,
            "expand": true
        });
        ws_write
            .send(Message::Text(ledger_req.to_string()))
            .await
            .map_err(|e| NodeError::Server(format!("failed to send ledger request: {e}")))?;

        // Wait for the response
        let response = self.read_response(ws_read).await?;

        let result = response
            .get("result")
            .or_else(|| response.get("ledger"))
            .unwrap_or(&response);

        let ledger_obj = result
            .get("ledger")
            .unwrap_or(result);

        // Extract ledger header blob
        let header_blob = ledger_obj
            .get("ledger_header")
            .and_then(|v| v.as_str())
            .and_then(|s| hex::decode(s).ok())
            .unwrap_or_else(|| {
                // Fall back to serializing the header fields as JSON
                serde_json::to_vec(ledger_obj).unwrap_or_default()
            });

        let hash_bytes = hex::decode(ledger_hash).unwrap_or_default();

        // Extract transactions
        let mut txs: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)> = Vec::new();
        if let Some(tx_array) = ledger_obj.get("transactions").and_then(|v| v.as_array()) {
            for tx_entry in tx_array {
                let tx_hash = tx_entry
                    .get("hash")
                    .and_then(|v| v.as_str())
                    .and_then(|s| hex::decode(s).ok())
                    .unwrap_or_default();

                let tx_blob = if let Some(blob_hex) = tx_entry.get("tx_blob").and_then(|v| v.as_str()) {
                    hex::decode(blob_hex).unwrap_or_default()
                } else if let Some(tx_obj) = tx_entry.get("tx") {
                    serde_json::to_vec(tx_obj).unwrap_or_default()
                } else {
                    // The entry itself might be the tx object
                    serde_json::to_vec(tx_entry).unwrap_or_default()
                };

                let meta_blob =
                    if let Some(meta_hex) = tx_entry.get("meta_blob").and_then(|v| v.as_str()) {
                        hex::decode(meta_hex).unwrap_or_default()
                    } else if let Some(meta_obj) = tx_entry.get("meta").or_else(|| tx_entry.get("metaData")) {
                        serde_json::to_vec(meta_obj).unwrap_or_default()
                    } else {
                        Vec::new()
                    };

                if !tx_hash.is_empty() {
                    txs.push((tx_hash, tx_blob, meta_blob));
                }
            }
        }

        self.receive_ledger(ledger_index, &hash_bytes, &header_blob, &txs)
            .await?;

        Ok(())
    }

    /// Read the next JSON response from the WebSocket stream, skipping
    /// subscription notifications.
    async fn read_response(
        &self,
        ws_read: &mut WsReader,
    ) -> Result<Value, NodeError> {
        let timeout = Duration::from_secs(30);
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let msg = tokio::time::timeout_at(deadline, ws_read.next())
                .await
                .map_err(|_| NodeError::Server("timeout waiting for ledger response".into()))?
                .ok_or_else(|| NodeError::Server("stream ended while waiting for response".into()))?
                .map_err(|e| NodeError::Server(format!("WebSocket error: {e}")))?;

            if let Message::Text(text) = msg {
                if let Ok(value) = serde_json::from_str::<Value>(&text) {
                    // Skip subscription events (they have a "type" field)
                    if value.get("type").is_some() && value.get("id").is_none() {
                        continue;
                    }
                    return Ok(value);
                }
            }
        }
    }
}

/// Compute exponential backoff delay for reconnection.
fn compute_backoff_delay(attempt: u32) -> Duration {
    let multiplied = RECONNECT_BACKOFF_MULTIPLIER.powi(attempt.min(31) as i32);
    let secs = INITIAL_RECONNECT_DELAY.as_secs_f64() * multiplied;
    let max_secs = MAX_RECONNECT_DELAY.as_secs_f64();
    Duration::from_secs_f64(secs.min(max_secs))
}

/// Extract affected accounts from a transaction blob and metadata.
///
/// Parses both as JSON and collects unique account identifiers from the
/// Account, Destination fields and AffectedNodes entries.
fn extract_affected_accounts(tx_blob: &[u8], meta_blob: &[u8]) -> Vec<Vec<u8>> {
    let mut accounts: Vec<Vec<u8>> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let mut add_account = |account_str: &str| {
        if let Ok(bytes) = hex::decode(account_str) {
            if seen.insert(bytes.clone()) {
                accounts.push(bytes);
            }
        } else {
            // Try decoding as a classic address
            if let Ok(account_id) = rxrpl_codec::address::classic::decode_account_id(account_str) {
                let bytes = account_id.0.to_vec();
                if seen.insert(bytes.clone()) {
                    accounts.push(bytes);
                }
            }
        }
    };

    // Parse tx blob for Account and Destination
    if let Ok(tx_json) = serde_json::from_slice::<Value>(tx_blob) {
        if let Some(account) = tx_json.get("Account").and_then(|v| v.as_str()) {
            add_account(account);
        }
        if let Some(dest) = tx_json.get("Destination").and_then(|v| v.as_str()) {
            add_account(dest);
        }
    }

    // Parse metadata for AffectedNodes
    if let Ok(meta_json) = serde_json::from_slice::<Value>(meta_blob) {
        if let Some(nodes) = meta_json.get("AffectedNodes").and_then(|v| v.as_array()) {
            for node in nodes {
                // Each node is wrapped in CreatedNode, ModifiedNode, or DeletedNode
                for wrapper_key in &["CreatedNode", "ModifiedNode", "DeletedNode"] {
                    if let Some(inner) = node.get(*wrapper_key) {
                        if let Some(fields) = inner
                            .get("FinalFields")
                            .or_else(|| inner.get("NewFields"))
                        {
                            if let Some(account) = fields.get("Account").and_then(|v| v.as_str()) {
                                add_account(account);
                            }
                        }
                    }
                }
            }
        }
    }

    accounts
}

impl super::Node {
    /// Run the node in reporting mode.
    ///
    /// Starts only the RPC server and the ETL pipeline. No consensus,
    /// no P2P overlay, no transaction processing. Write requests received
    /// by the RPC server are forwarded to the configured upstream node.
    pub async fn run_reporting(&self) -> Result<(), NodeError> {
        let reporting_cfg = &self.config.reporting;
        if !reporting_cfg.enabled {
            return Err(NodeError::Config(
                "reporting mode is not enabled in configuration".into(),
            ));
        }

        let bind = self.config.server.bind;
        let forward_url = reporting_cfg.forward_url.clone();
        let etl_source = reporting_cfg.etl_source.clone();

        // Create ledger store for reporting data
        let ledger_store: Arc<dyn LedgerStore> = {
            let db_path = self.config.database.path.join("reporting.db");
            Arc::new(
                rxrpl_storage::SqliteLedgerStore::open(&db_path).unwrap_or_else(|_| {
                    rxrpl_storage::SqliteLedgerStore::in_memory()
                        .expect("in-memory fallback")
                }),
            )
        };

        // Build RPC server context in reporting mode with the ledger store
        let ctx = ServerContext::for_reporting(
            self.config.server.clone(),
            forward_url.clone(),
            ledger_store.clone(),
        );
        let app = rxrpl_rpc_server::build_router(ctx);

        tracing::info!("starting reporting-mode RPC server on {}", bind);
        tracing::info!("forwarding write requests to {}", forward_url);

        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        // Spawn RPC server
        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("reporting RPC server error: {}", e);
            }
        });

        // Start ETL pipeline
        let pipeline = EtlPipeline::new(etl_source, ledger_store);
        pipeline.run().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_storage::InMemoryLedgerStore;

    #[tokio::test]
    async fn receive_ledger_stores_header_and_txs() {
        let store = Arc::new(InMemoryLedgerStore::new());
        let pipeline = EtlPipeline::new("ws://localhost:6006".into(), store.clone());

        let hash = b"ledger_hash_32bytes_placeholder!";
        let header = b"serialized_header_data";
        let tx_hash = b"tx_hash_placeholder_here!!!!!!!!".to_vec();
        let tx_blob = b"{\"Account\":\"rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh\"}".to_vec();
        let meta_blob = b"{\"AffectedNodes\":[]}".to_vec();

        let txs = vec![(tx_hash.clone(), tx_blob, meta_blob)];
        pipeline
            .receive_ledger(42, hash, header, &txs)
            .await
            .unwrap();

        assert_eq!(pipeline.last_stored_seq().await, Some(42));

        let stored = store.get_ledger_header(42).unwrap().unwrap();
        assert_eq!(stored.sequence, 42);
        assert_eq!(stored.hash, hash.to_vec());

        let tx = store.get_tx(&tx_hash).unwrap().unwrap();
        assert_eq!(tx.ledger_seq, 42);
    }

    #[tokio::test]
    async fn receive_ledger_indexes_accounts() {
        let store = Arc::new(InMemoryLedgerStore::new());
        let pipeline = EtlPipeline::new("ws://localhost:6006".into(), store.clone());

        let hash = b"ledger_hash_32bytes_placeholder!";
        let header = b"header";
        // Use a classic address in the Account field
        let tx_blob = serde_json::json!({
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe"
        });
        let tx_blob_bytes = serde_json::to_vec(&tx_blob).unwrap();
        let meta_blob = b"{}".to_vec();
        let tx_hash = b"tx_hash_for_account_index_test!!".to_vec();

        let txs = vec![(tx_hash.clone(), tx_blob_bytes, meta_blob)];
        pipeline
            .receive_ledger(10, hash, header, &txs)
            .await
            .unwrap();

        // Verify account indexing happened by checking the Account address
        let account_id =
            rxrpl_codec::address::classic::decode_account_id("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh")
                .unwrap();
        let results = store.get_account_txs(&account_id.0, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tx_hash, tx_hash);
    }

    #[test]
    fn extract_affected_accounts_from_tx() {
        let tx_blob = serde_json::json!({
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Destination": "rPT1Sjq2YGrBMTttX4GZHjKu9dyfzbpAYe"
        });
        let tx_bytes = serde_json::to_vec(&tx_blob).unwrap();
        let meta_bytes = b"{}";

        let accounts = extract_affected_accounts(&tx_bytes, meta_bytes);
        assert_eq!(accounts.len(), 2);
    }

    #[test]
    fn extract_affected_accounts_deduplicates() {
        let tx_blob = serde_json::json!({
            "Account": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh",
            "Destination": "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh"
        });
        let tx_bytes = serde_json::to_vec(&tx_blob).unwrap();
        let meta_bytes = b"{}";

        let accounts = extract_affected_accounts(&tx_bytes, meta_bytes);
        assert_eq!(accounts.len(), 1);
    }

    #[test]
    fn compute_backoff_respects_max() {
        let delay = compute_backoff_delay(100);
        assert!(delay <= MAX_RECONNECT_DELAY);
    }

    #[test]
    fn compute_backoff_increases() {
        let d0 = compute_backoff_delay(0);
        let d1 = compute_backoff_delay(1);
        let d2 = compute_backoff_delay(2);
        assert!(d1 > d0);
        assert!(d2 > d1);
    }
}
