use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
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
    pub validator_list_status:
        Option<Arc<RwLock<serde_json::Value>>>,
    /// Configured network id (e.g. 0 = mainnet, 21337 = devnet, 10000 = test).
    /// Used by `sign` to auto-fill `NetworkID` on transactions, which modern
    /// rippled requires (`telREQUIRES_NETWORK_ID`).
    pub network_id: Option<u32>,
    /// Local validator manifest (this node's own), set once at boot from
    /// the loaded `ValidatorIdentity`. Populated by Node::run_networked
    /// via `set_local_manifest`. Reads use the snapshot fields directly;
    /// the OnceLock makes the post-Arc-construction set possible without
    /// interior mutability for every field.
    local_manifest: OnceLock<Arc<LocalManifestSnapshot>>,
    event_tx: broadcast::Sender<ServerEvent>,
}

/// Snapshot of the local validator manifest exposed via the `manifest` RPC.
///
/// Lives in `rxrpl-rpc-server` (rather than depending on
/// `rxrpl-overlay::manifest::Manifest`) to keep this crate's dep graph
/// lightweight — Node converts overlay's full Manifest into a snapshot at
/// boot.
#[derive(Clone, Debug)]
pub struct LocalManifestSnapshot {
    pub master_public_key: Vec<u8>,
    pub ephemeral_public_key: Vec<u8>,
    pub sequence: u32,
    pub domain: Option<String>,
    /// Raw signed-STObject bytes (what the manifest RPC returns base64-encoded).
    pub raw_bytes: Vec<u8>,
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
            network_id: None,
            local_manifest: OnceLock::new(),
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
            network_id: None,
            local_manifest: OnceLock::new(),
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
            network_id: None,
            local_manifest: OnceLock::new(),
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
            network_id: None,
            local_manifest: OnceLock::new(),
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
            network_id: None,
            local_manifest: OnceLock::new(),
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
    pub fn attach_network_id(self: &mut Arc<Self>, network_id: u32) {
        if let Some(ctx) = Arc::get_mut(self) {
            ctx.network_id = Some(network_id);
        }
    }

    /// Get a reference to the event broadcast sender.
    pub fn event_sender(&self) -> &broadcast::Sender<ServerEvent> {
        &self.event_tx
    }

    /// Set the local validator manifest (this node's own). Idempotent
    /// once set — subsequent calls are no-ops, since the manifest is
    /// loaded once at boot from `ValidatorIdentity`.
    ///
    /// Returns `Ok(())` if the snapshot was stored, `Err(_)` if a
    /// manifest was already set (the existing one is unchanged).
    pub fn set_local_manifest(
        &self,
        snapshot: LocalManifestSnapshot,
    ) -> Result<(), Arc<LocalManifestSnapshot>> {
        self.local_manifest.set(Arc::new(snapshot))
    }

    /// Get the local validator manifest snapshot, if set.
    pub fn local_manifest(&self) -> Option<&LocalManifestSnapshot> {
        self.local_manifest.get().map(|arc| arc.as_ref())
    }
}
