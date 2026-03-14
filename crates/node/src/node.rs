use std::sync::Arc;

use rxrpl_amendment::{AmendmentTable, FeatureRegistry};
use rxrpl_config::NodeConfig;
use rxrpl_ledger::Ledger;
use rxrpl_rpc_server::ServerContext;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use rxrpl_txq::TxQueue;
use tokio::sync::RwLock;

use crate::error::NodeError;

/// The top-level XRPL node.
///
/// Wires together all subsystems: storage, ledger, transaction engine,
/// mempool, consensus, overlay, and RPC server.
#[allow(dead_code)]
pub struct Node {
    config: NodeConfig,
    ledger: Arc<RwLock<Ledger>>,
    tx_engine: Arc<TxEngine>,
    tx_queue: Arc<RwLock<TxQueue>>,
    amendment_table: Arc<RwLock<AmendmentTable>>,
    fees: Arc<FeeSettings>,
    running: bool,
}

impl Node {
    /// Create a new node from configuration.
    pub fn new(config: NodeConfig) -> Result<Self, NodeError> {
        // Initialize amendment registry
        let registry = FeatureRegistry::with_known_amendments();
        let amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4); // ~14 days at 4s/ledger

        // Initialize transaction engine with Phase A handlers
        let mut tx_registry = TransactorRegistry::new();
        rxrpl_tx_engine::handlers::register_phase_a(&mut tx_registry);
        let tx_engine = TxEngine::new(tx_registry);

        // Initialize genesis ledger
        let ledger = Ledger::genesis();

        // Initialize transaction queue
        let tx_queue = TxQueue::new(2000);

        Ok(Self {
            config,
            ledger: Arc::new(RwLock::new(ledger)),
            tx_engine: Arc::new(tx_engine),
            tx_queue: Arc::new(RwLock::new(tx_queue)),
            amendment_table: Arc::new(RwLock::new(amendment_table)),
            fees: Arc::new(FeeSettings::default()),
            running: false,
        })
    }

    /// Start the node (RPC server and peer networking).
    pub async fn start(&mut self) -> Result<(), NodeError> {
        if self.running {
            return Err(NodeError::AlreadyRunning);
        }

        let ctx = ServerContext::new(self.config.server.clone());
        let app = rxrpl_rpc_server::build_router(ctx);
        let bind = self.config.server.bind;

        tracing::info!("starting RPC server on {}", bind);

        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        self.running = true;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("RPC server error: {}", e);
            }
        });

        tracing::info!("node started");
        Ok(())
    }

    /// Get a reference to the current ledger.
    pub fn ledger(&self) -> &Arc<RwLock<Ledger>> {
        &self.ledger
    }

    /// Get a reference to the transaction engine.
    pub fn tx_engine(&self) -> &Arc<TxEngine> {
        &self.tx_engine
    }

    /// Get a reference to the fees.
    pub fn fees(&self) -> &Arc<FeeSettings> {
        &self.fees
    }

    /// Check if the node is running.
    pub fn is_running(&self) -> bool {
        self.running
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_node() {
        let config = NodeConfig::default();
        let node = Node::new(config).unwrap();
        assert!(!node.is_running());
    }
}
