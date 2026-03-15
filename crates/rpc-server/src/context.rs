use std::collections::VecDeque;
use std::sync::Arc;

use rxrpl_config::ServerConfig;
use rxrpl_ledger::Ledger;
use rxrpl_tx_engine::{FeeSettings, TxEngine};
use tokio::sync::RwLock;

/// Shared state for all RPC handlers.
pub struct ServerContext {
    pub config: ServerConfig,
    pub ledger: Option<Arc<RwLock<Ledger>>>,
    pub closed_ledgers: Option<Arc<RwLock<VecDeque<Ledger>>>>,
    pub tx_engine: Option<Arc<TxEngine>>,
    pub fees: Option<Arc<FeeSettings>>,
}

impl ServerContext {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            ledger: None,
            closed_ledgers: None,
            tx_engine: None,
            fees: None,
        })
    }

    /// Create a context with full node state for standalone mode.
    pub fn with_node_state(
        config: ServerConfig,
        ledger: Arc<RwLock<Ledger>>,
        closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
        tx_engine: Arc<TxEngine>,
        fees: Arc<FeeSettings>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            ledger: Some(ledger),
            closed_ledgers: Some(closed_ledgers),
            tx_engine: Some(tx_engine),
            fees: Some(fees),
        })
    }
}
