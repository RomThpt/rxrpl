use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use rxrpl_config::ServerConfig;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;
use rxrpl_storage::SqliteStore;
use rxrpl_tx_engine::{FeeSettings, TxEngine};
use rxrpl_txq::TxQueue;
use tokio::sync::{RwLock, broadcast, mpsc};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::events::ServerEvent;

/// Shared state for all RPC handlers.
pub struct ServerContext {
    pub config: ServerConfig,
    pub ledger: Option<Arc<RwLock<Ledger>>>,
    pub closed_ledgers: Option<Arc<RwLock<VecDeque<Ledger>>>>,
    pub tx_engine: Option<Arc<TxEngine>>,
    pub fees: Option<Arc<FeeSettings>>,
    pub tx_store: Option<Arc<SqliteStore>>,
    pub tx_queue: Option<Arc<RwLock<TxQueue>>>,
    pub relay_tx: Option<mpsc::UnboundedSender<(Hash256, Vec<u8>)>>,
    pub metrics_handle: Option<PrometheusHandle>,
    pub peer_reservations: Arc<RwLock<HashSet<String>>>,
    event_tx: broadcast::Sender<ServerEvent>,
}

impl ServerContext {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            config,
            ledger: None,
            closed_ledgers: None,
            tx_engine: None,
            fees: None,
            tx_store: None,
            tx_queue: None,
            relay_tx: None,
            metrics_handle: None,
            peer_reservations: Arc::new(RwLock::new(HashSet::new())),
            event_tx,
        })
    }

    /// Create a context with full node state for standalone mode.
    #[allow(clippy::too_many_arguments)]
    pub fn with_node_state(
        config: ServerConfig,
        ledger: Arc<RwLock<Ledger>>,
        closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
        tx_engine: Arc<TxEngine>,
        fees: Arc<FeeSettings>,
        tx_store: Option<Arc<SqliteStore>>,
        tx_queue: Option<Arc<RwLock<TxQueue>>>,
        relay_tx: Option<mpsc::UnboundedSender<(Hash256, Vec<u8>)>>,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            config,
            ledger: Some(ledger),
            closed_ledgers: Some(closed_ledgers),
            tx_engine: Some(tx_engine),
            fees: Some(fees),
            tx_store,
            tx_queue,
            relay_tx,
            metrics_handle: None,
            peer_reservations: Arc::new(RwLock::new(HashSet::new())),
            event_tx,
        })
    }

    /// Create a context with metrics handle.
    #[allow(clippy::too_many_arguments)]
    pub fn with_node_state_and_metrics(
        config: ServerConfig,
        ledger: Arc<RwLock<Ledger>>,
        closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
        tx_engine: Arc<TxEngine>,
        fees: Arc<FeeSettings>,
        tx_store: Option<Arc<SqliteStore>>,
        tx_queue: Option<Arc<RwLock<TxQueue>>>,
        relay_tx: Option<mpsc::UnboundedSender<(Hash256, Vec<u8>)>>,
        metrics_handle: PrometheusHandle,
    ) -> Arc<Self> {
        let (event_tx, _) = broadcast::channel(1024);
        Arc::new(Self {
            config,
            ledger: Some(ledger),
            closed_ledgers: Some(closed_ledgers),
            tx_engine: Some(tx_engine),
            fees: Some(fees),
            tx_store,
            tx_queue,
            relay_tx,
            metrics_handle: Some(metrics_handle),
            peer_reservations: Arc::new(RwLock::new(HashSet::new())),
            event_tx,
        })
    }

    /// Get a reference to the event broadcast sender.
    pub fn event_sender(&self) -> &broadcast::Sender<ServerEvent> {
        &self.event_tx
    }
}
