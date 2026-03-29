use std::net::SocketAddr;
use std::sync::Arc;

use rxrpl_rpc_server::ServerContext;
use rxrpl_storage::{InMemoryLedgerStore, LedgerStore};
use tokio::sync::RwLock;

use crate::error::NodeError;

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
    /// In a full implementation this would parse the WebSocket message,
    /// extract the ledger header and transactions, and persist them.
    /// For now it accepts pre-parsed data.
    pub async fn receive_ledger(
        &self,
        seq: u32,
        hash: &[u8],
        header_blob: &[u8],
        txs: &[(Vec<u8>, Vec<u8>, Vec<u8>)], // (tx_hash, tx_blob, meta_blob)
    ) -> Result<(), NodeError> {
        self.ledger_store
            .store_ledger(seq, hash, header_blob)
?;

        for (tx_hash, tx_blob, meta_blob) in txs {
            self.ledger_store
                .store_tx(tx_hash, seq, tx_blob, meta_blob)?;
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
    /// Connects to the upstream source and continuously receives validated
    /// ledgers. This is a placeholder that logs the connection attempt;
    /// full WebSocket subscription will be wired in a follow-up.
    pub async fn run(&self) -> Result<(), NodeError> {
        tracing::info!(
            source = %self.etl_source,
            "ETL pipeline started (awaiting upstream connection implementation)"
        );

        // Placeholder: in production this would open a WebSocket to
        // self.etl_source, subscribe to the "ledger" stream, and call
        // self.receive_ledger() for each validated ledger received.
        //
        // For now we just park so the task stays alive.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            tracing::debug!(source = %self.etl_source, "ETL pipeline heartbeat");
        }
    }
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
        let ledger_store: Arc<dyn LedgerStore> = Arc::new(InMemoryLedgerStore::new());

        // Build RPC server context in reporting mode
        let ctx = ServerContext::for_reporting(self.config.server.clone(), forward_url.clone());
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
