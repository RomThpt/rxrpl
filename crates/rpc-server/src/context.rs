use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use rxrpl_config::ServerConfig;
use rxrpl_ledger::Ledger;
use rxrpl_nodestore::ShardManager;
use rxrpl_primitives::Hash256;
use rxrpl_storage::{LedgerStore, TxStore};
use rxrpl_tx_engine::{FeeSettings, TxEngine};
use rxrpl_txq::TxQueue;
use tokio::sync::{RwLock, broadcast, mpsc};

use metrics_exporter_prometheus::PrometheusHandle;

use crate::events::ServerEvent;

/// Shared pruner state accessible from RPC handlers.
///
/// Wraps atomic counters so the pruner can be queried and controlled
/// from `can_delete` and `ledger_cleaner` without holding a lock.
pub struct PrunerState {
    /// Earliest ledger still available after pruning.
    pub earliest_seq: AtomicU32,
    /// Advisory delete cursor (max sequence eligible for deletion).
    pub can_delete_seq: AtomicU32,
    /// Whether advisory delete mode is active.
    pub advisory_delete: bool,
    /// Retention window size.
    pub retention_window: u32,
}

impl PrunerState {
    pub fn new(retention_window: u32, advisory_delete: bool) -> Self {
        Self {
            earliest_seq: AtomicU32::new(0),
            can_delete_seq: AtomicU32::new(if advisory_delete { 0 } else { u32::MAX }),
            advisory_delete,
            retention_window,
        }
    }
}

/// Shared state for all RPC handlers.
pub struct ServerContext {
    pub config: ServerConfig,
    pub ledger: Option<Arc<RwLock<Ledger>>>,
    pub closed_ledgers: Option<Arc<RwLock<VecDeque<Ledger>>>>,
    pub tx_engine: Option<Arc<TxEngine>>,
    pub fees: Option<Arc<FeeSettings>>,
    pub tx_store: Option<Arc<dyn TxStore>>,
    pub tx_queue: Option<Arc<RwLock<TxQueue>>>,
    pub relay_tx: Option<mpsc::UnboundedSender<(Hash256, Vec<u8>)>>,
    pub metrics_handle: Option<PrometheusHandle>,
    pub peer_reservations: Arc<RwLock<HashSet<String>>>,
    pub pruner_state: Option<Arc<PrunerState>>,
    pub shard_manager: Option<Arc<RwLock<ShardManager>>>,
    /// Ledger store for reporting mode (historical ledger and tx data).
    pub ledger_store: Option<Arc<dyn LedgerStore>>,
    /// Whether the node is running in reporting mode (read-only, no consensus).
    pub reporting_mode: bool,
    /// URL to forward write requests to when in reporting mode.
    pub forward_url: Option<String>,
    /// Live status snapshot of the UNL fetcher, populated by the overlay
    /// layer when `validator_list_sites` are configured. Read by the
    /// `validator_list_sites` RPC method.
    pub validator_list_status: Option<Arc<RwLock<serde_json::Value>>>,
    /// Live snapshot of validator domain attestation status, populated by
    /// the overlay layer's `DomainAttestationService`. Read by the
    /// `server_info` and `validators` RPC methods. Shape:
    /// ```json
    /// {
    ///   "local": {"verified": bool, "domain": str, "status": "verified"|...,
    ///             "last_check": u64},
    ///   "validators": [{"public_key": "ED..", "domain": "...",
    ///                   "verification_status": "verified"|..,
    ///                   "last_verified": u64}]
    /// }
    /// ```
    pub domain_attestation_status: Option<Arc<RwLock<serde_json::Value>>>,
    /// Configured network id (e.g. 0 = mainnet, 21337 = devnet, 10000 = test).
    /// Used by `sign` to auto-fill `NetworkID` on transactions, which modern
    /// rippled requires (`telREQUIRES_NETWORK_ID`).
    pub network_id: Option<u32>,
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
            pruner_state: None,
            shard_manager: None,
            ledger_store: None,
            reporting_mode: false,
            forward_url: None,
            validator_list_status: None,
            domain_attestation_status: None,
            network_id: None,
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
        tx_store: Option<Arc<dyn TxStore>>,
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
            pruner_state: None,
            shard_manager: None,
            ledger_store: None,
            reporting_mode: false,
            forward_url: None,
            validator_list_status: None,
            domain_attestation_status: None,
            network_id: None,
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
        tx_store: Option<Arc<dyn TxStore>>,
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
            pruner_state: None,
            shard_manager: None,
            ledger_store: None,
            reporting_mode: false,
            forward_url: None,
            validator_list_status: None,
            domain_attestation_status: None,
            network_id: None,
            event_tx,
        })
    }

    /// Create a context with full node state and pruner state.
    #[allow(clippy::too_many_arguments)]
    pub fn with_node_state_and_pruner(
        config: ServerConfig,
        ledger: Arc<RwLock<Ledger>>,
        closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
        tx_engine: Arc<TxEngine>,
        fees: Arc<FeeSettings>,
        tx_store: Option<Arc<dyn TxStore>>,
        tx_queue: Option<Arc<RwLock<TxQueue>>>,
        relay_tx: Option<mpsc::UnboundedSender<(Hash256, Vec<u8>)>>,
        pruner_state: Arc<PrunerState>,
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
            pruner_state: Some(pruner_state),
            shard_manager: None,
            ledger_store: None,
            reporting_mode: false,
            forward_url: None,
            validator_list_status: None,
            domain_attestation_status: None,
            network_id: None,
            event_tx,
        })
    }

    /// Create a context for reporting mode (read-only, no consensus).
    pub fn for_reporting(
        config: ServerConfig,
        forward_url: String,
        ledger_store: Arc<dyn LedgerStore>,
    ) -> Arc<Self> {
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
            pruner_state: None,
            shard_manager: None,
            ledger_store: Some(ledger_store),
            reporting_mode: true,
            forward_url: Some(forward_url),
            validator_list_status: None,
            domain_attestation_status: None,
            network_id: None,
            event_tx,
        })
    }

    /// Attach a UNL status handle. Must be called *before* the context is
    /// shared with other tasks (i.e. while the strong count is 1), otherwise
    /// the call is a silent no-op.
    pub fn attach_validator_list_status(
        self: &mut Arc<Self>,
        handle: Arc<RwLock<serde_json::Value>>,
    ) {
        if let Some(ctx) = Arc::get_mut(self) {
            ctx.validator_list_status = Some(handle);
        }
    }

    /// Attach the configured network id so `sign` can auto-fill
    /// `NetworkID` on outbound transactions. Same get_mut constraint as
    /// `attach_validator_list_status`.
    /// Attach the shared domain attestation status snapshot. Same get_mut
    /// constraint as `attach_validator_list_status`.
    pub fn attach_domain_attestation_status(
        self: &mut Arc<Self>,
        handle: Arc<RwLock<serde_json::Value>>,
    ) {
        if let Some(ctx) = Arc::get_mut(self) {
            ctx.domain_attestation_status = Some(handle);
        }
    }

    pub fn attach_network_id(self: &mut Arc<Self>, network_id: u32) {
        if let Some(ctx) = Arc::get_mut(self) {
            ctx.network_id = Some(network_id);
        }
    }

    /// Get a reference to the event broadcast sender.
    pub fn event_sender(&self) -> &broadcast::Sender<ServerEvent> {
        &self.event_tx
    }
}
