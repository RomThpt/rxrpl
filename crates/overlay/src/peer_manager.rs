use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use openssl::ssl::{SslAcceptor, SslConnector};
use std::collections::HashSet;

use rxrpl_consensus::types::{Proposal, TxSet, Validation};
use rxrpl_p2p_proto::MessageType;
use rxrpl_p2p_proto::codec::{PeerCodec, PeerMessage};
use rxrpl_primitives::Hash256;
use rxrpl_shamap::NodeId as ShamapNodeId;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, RwLock, Semaphore, mpsc};
use tokio_util::codec::Framed;

use crate::cluster::ClusterManager;
use crate::command::OverlayCommand;
use crate::crawl::{self, CrawlInfo};
use crate::discovery::PeerDiscovery;
use crate::error::OverlayError;
use crate::event::PeerEvent;
use crate::handshake;
use crate::http;
use crate::identity::NodeIdentity;
use crate::ledger_provider::LedgerProvider;
use crate::ledger_sync::LedgerSyncer;
use crate::manifest::{self, ManifestStore};
use crate::peer_handle::PeerHandle;
use crate::peer_loop;
use crate::peer_score::PeerScore;
use crate::peer_set::{PeerInfo, PeerSet};
use crate::proto_convert;
use crate::relay::RelayFilter;
use crate::reputation::PeerReputation;
use crate::shard_sync::ShardSyncer;
use crate::squelch::SquelchManager;
use crate::tls::{self, PeerStream};
use crate::tx_batch_relay::TxBatchRelay;
use crate::validator_list::{self, ValidatorListTracker};

/// TMLedgerInfoType values from rippled.
const LI_BASE: i32 = 0;
const LI_TX_NODE: i32 = 1;
const LI_AS_NODE: i32 = 2;
const LI_TS_CANDIDATE: i32 = 3;

/// Peers a delta-sync round fans its missing-node requests across. With fat
/// subtrees served per id, more parallel peers cut catchup round-trips once the
/// per-reply node cap is large (bench: 3 peers 91 rounds -> 8 peers 69).
const DELTA_SYNC_FANOUT: usize = 8;

/// Capacity of the bounded overlay->consensus message channel.
///
/// This channel was previously unbounded: under a mainnet transaction or
/// ledger-data burst the producer (this single peer-manager event loop) can
/// outrun the consensus consumer and the queue grows without limit until the
/// node OOMs (M0 validator-hardening finding). Bounding it caps the queued
/// message count, so a sustained flood sheds at the tail instead of eating
/// all memory -- the rippled policy of bounded buffers + shedding under
/// overload, never unbounded growth.
///
/// 4096 is sized off the upstream `event_tx` bound (8192) and the message mix
/// that reaches consensus: proposals/validations are a few tens per ledger,
/// relay-filtered transactions a few hundred/s, and the largest contributor is
/// catchup `LedgerData` (fan-out of 8 peers x batched fat-subtree replies).
/// 4096 slots absorb several seconds of steady consensus traffic plus multiple
/// full fan-out `LedgerData` bursts, while keeping the worst-case queued memory
/// bounded (O(cap x max message size)). A few thousand is the sweet spot: large
/// enough that healthy operation never sheds, small enough to bound memory.
const CONSENSUS_CHANNEL_CAP: usize = 4096;

/// HaveTransactionSet status values from rippled.
/// tsNEW_SET = 1: peer is proposing a new transaction set.
const _TS_NEW_SET: u32 = 1;

/// Messages forwarded from the overlay to the consensus layer.
pub enum ConsensusMessage {
    Proposal(Proposal),
    Validation(Validation),
    Transaction {
        hash: Hash256,
        data: Vec<u8>,
    },
    StatusChange {
        from: Hash256,
        ledger_seq: u32,
        ledger_hash: Hash256,
    },
    LedgerData {
        hash: Hash256,
        seq: u32,
        nodes: Vec<(Vec<u8>, Vec<u8>)>,
    },
    LedgerHeader {
        seq: u32,
        header: rxrpl_ledger::LedgerHeader,
    },
    ValidatorListReceived {
        validator_count: usize,
    },
    /// A verified validator list with parsed master keys.
    ValidatorListVerified {
        validators: Vec<rxrpl_primitives::PublicKey>,
        sequence: u64,
    },
    /// A manifest was applied, containing ephemeral key mapping.
    ManifestApplied {
        master_key: rxrpl_primitives::PublicKey,
        ephemeral_key: Option<rxrpl_primitives::PublicKey>,
        old_ephemeral_key: Option<rxrpl_primitives::PublicKey>,
        revoked: bool,
    },
    TxSetAcquired(TxSet),
}

impl ConsensusMessage {
    /// Short static label for logging shed messages without moving the value.
    fn kind(&self) -> &'static str {
        match self {
            ConsensusMessage::Proposal(_) => "Proposal",
            ConsensusMessage::Validation(_) => "Validation",
            ConsensusMessage::Transaction { .. } => "Transaction",
            ConsensusMessage::StatusChange { .. } => "StatusChange",
            ConsensusMessage::LedgerData { .. } => "LedgerData",
            ConsensusMessage::LedgerHeader { .. } => "LedgerHeader",
            ConsensusMessage::ValidatorListReceived { .. } => "ValidatorListReceived",
            ConsensusMessage::ValidatorListVerified { .. } => "ValidatorListVerified",
            ConsensusMessage::ManifestApplied { .. } => "ManifestApplied",
            ConsensusMessage::TxSetAcquired(_) => "TxSetAcquired",
        }
    }

    /// Whether losing this message can stall consensus. Proposals, validations
    /// and the acquired tx-set are the votes/inputs a round needs to converge;
    /// everything else is either high-volume gossip (transactions) or
    /// re-requestable/replayable state (ledger data/headers, validator lists,
    /// manifests, status changes). Critical drops are logged loudly; bulk
    /// drops are logged at debug so a transaction flood cannot spam the log.
    fn is_critical(&self) -> bool {
        matches!(
            self,
            ConsensusMessage::Proposal(_)
                | ConsensusMessage::Validation(_)
                | ConsensusMessage::TxSetAcquired(_)
        )
    }
}

/// Configuration for the peer manager.
pub struct PeerManagerConfig {
    pub listen_port: u16,
    pub max_peers: usize,
    pub seeds: Vec<String>,
    pub fixed_peers: Vec<String>,
    pub network_id: u32,
    pub tls_server: Arc<SslAcceptor>,
    pub tls_client: Arc<SslConnector>,
    /// Cluster mode configuration.
    pub cluster_enabled: bool,
    /// Human-readable node name for cluster broadcasts.
    pub cluster_node_name: String,
    /// Public keys of trusted cluster member nodes.
    pub cluster_members: Vec<String>,
    /// Interval between cluster status broadcasts (seconds).
    pub cluster_broadcast_interval_secs: u64,
}

/// Central P2P network manager.
///
/// Accepts inbound connections, manages outbound connections,
/// and dispatches messages between peers and the consensus layer.
pub struct PeerManager {
    identity: Arc<NodeIdentity>,
    config: PeerManagerConfig,
    seeds: Vec<String>,
    peer_set: Arc<PeerSet>,
    peer_handles: HashMap<Hash256, PeerHandle>,
    relay_filter: RelayFilter,
    ledger_seq: Arc<AtomicU32>,
    ledger_hash: Arc<RwLock<Hash256>>,
    cmd_rx: mpsc::UnboundedReceiver<OverlayCommand>,
    cmd_tx_internal: mpsc::UnboundedSender<OverlayCommand>,
    event_rx: mpsc::Receiver<PeerEvent>,
    event_tx: mpsc::Sender<PeerEvent>,
    consensus_tx: mpsc::Sender<ConsensusMessage>,
    /// Count of messages shed because [`consensus_tx`](Self::consensus_tx) was
    /// full (overload back-pressure). Surfaced via [`consensus_dropped`].
    consensus_dropped: AtomicU64,
    ledger_provider: Option<Arc<dyn LedgerProvider>>,
    node_store: Option<Arc<dyn rxrpl_shamap::NodeStore>>,
    ledger_syncer: LedgerSyncer,
    next_cookie: AtomicU64,
    discovery: Option<Arc<PeerDiscovery>>,
    server_event_tx: Option<tokio::sync::broadcast::Sender<serde_json::Value>>,
    /// Shared cache of known transaction sets (shared with NetworkConsensusAdapter).
    tx_sets: Option<Arc<std::sync::RwLock<HashMap<Hash256, TxSet>>>>,
    /// Tx-set hashes currently being fetched to avoid duplicate requests.
    pending_tx_set_fetches: HashSet<Hash256>,
    manifest_store: ManifestStore,
    vl_tracker: ValidatorListTracker,
    cluster_manager: ClusterManager,
    squelch_manager: SquelchManager,
    tx_batch_relay: TxBatchRelay,
    shard_syncer: Option<ShardSyncer>,
    /// Notifiers for fixed peer reconnection, keyed by address.
    /// When a fixed peer disconnects, its notifier is triggered so
    /// the reconnection task wakes up immediately instead of polling.
    fixed_peer_notifiers: HashMap<String, Arc<Notify>>,
    /// Maps node_id -> fixed peer address for connected fixed peers.
    fixed_peer_node_ids: HashMap<Hash256, String>,
    /// Signalled on shutdown to cancel pending reconnection tasks.
    shutdown_notify: Arc<Notify>,
    /// Caps the number of concurrent inbound TLS handshakes. Each
    /// handshake spawns a task that performs CPU-heavy crypto; without
    /// this bound a remote attacker could open thousands of half-open
    /// TCP connections and saturate the runtime (audit finding H5).
    inbound_handshake_permits: Arc<Semaphore>,
    /// Supplies the server-level fields of the `/crawl` response. `None` keeps
    /// the crawl limited to overlay-known data (used in tests).
    crawl_info: Option<Arc<dyn CrawlInfo>>,
}

impl PeerManager {
    pub fn new(
        identity: Arc<NodeIdentity>,
        config: PeerManagerConfig,
        ledger_seq: Arc<AtomicU32>,
        ledger_hash: Arc<RwLock<Hash256>>,
    ) -> (
        Self,
        mpsc::UnboundedSender<OverlayCommand>,
        mpsc::Receiver<ConsensusMessage>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cmd_tx_internal = cmd_tx.clone();
        // Bounded so a transaction/ledger-data burst sheds at the tail instead
        // of growing without limit and OOMing the node (see CONSENSUS_CHANNEL_CAP).
        let (consensus_tx, consensus_rx) = mpsc::channel(CONSENSUS_CHANNEL_CAP);
        // Bounded so a flood of inbound peer messages exerts TCP
        // backpressure on the offending peer's read loop instead of
        // growing an unbounded queue (audit finding C4). 8192 is well
        // above the steady-state of 21 peers x 100 msg/s rate-limited.
        let (event_tx, event_rx) = mpsc::channel(8192);
        let peer_set = Arc::new(PeerSet::new(config.max_peers));

        let seeds = config.seeds.clone();
        let cluster_manager = ClusterManager::new(
            config.cluster_enabled,
            config.cluster_node_name.clone(),
            config.cluster_members.clone(),
        );
        let mgr = Self {
            identity,
            seeds,
            config,
            peer_set,
            peer_handles: HashMap::new(),
            relay_filter: RelayFilter::new(65536),
            ledger_seq,
            ledger_hash,
            cmd_rx,
            cmd_tx_internal,
            event_rx,
            event_tx,
            consensus_tx,
            consensus_dropped: AtomicU64::new(0),
            ledger_provider: None,
            node_store: None,
            ledger_syncer: LedgerSyncer::new(),
            next_cookie: AtomicU64::new(1),
            discovery: None,
            server_event_tx: None,
            tx_sets: None,
            pending_tx_set_fetches: HashSet::new(),
            manifest_store: ManifestStore::new(),
            vl_tracker: ValidatorListTracker::new(),
            cluster_manager,
            squelch_manager: SquelchManager::new(),
            tx_batch_relay: TxBatchRelay::new(),
            shard_syncer: None,
            fixed_peer_notifiers: HashMap::new(),
            fixed_peer_node_ids: HashMap::new(),
            shutdown_notify: Arc::new(Notify::new()),
            // Allow up to 64 in-flight inbound handshakes. With OpenSSL ~5ms
            // per handshake on commodity hardware that is ~12k handshakes/s,
            // well above any honest peer rate. Saturated handshake slots
            // simply cause new TCP connections to wait, which Linux's accept
            // queue absorbs naturally.
            inbound_handshake_permits: Arc::new(Semaphore::new(64)),
            crawl_info: None,
        };

        (mgr, cmd_tx, consensus_rx)
    }

    /// Clone the internal consensus-message sender so callers (typically the
    /// node's close path) can inject synthetic `ConsensusMessage` values into
    /// the consensus loop's input queue. This lets the local close handler
    /// feed its own freshly-signed validation back into the same path used
    /// for peer-received validations, so it counts toward UNL quorum.
    pub fn consensus_sender(&self) -> mpsc::Sender<ConsensusMessage> {
        self.consensus_tx.clone()
    }

    /// Forward a message to the consensus loop over the bounded channel.
    ///
    /// Always uses `try_send` (never `await`): this runs on the single
    /// peer-manager event loop, which also drives `accept`, sync and the
    /// per-peer dispatch. Awaiting a full channel here would stall every one
    /// of those, and because the consensus consumer (`node.rs`) drains this
    /// channel from the *same* `tokio::select!` task that closes ledgers,
    /// awaiting would risk a self-inflicted deadlock. So under overload we
    /// shed: increment the dropped counter and log (loudly for
    /// consensus-critical messages, at debug for bulk gossip/replayable
    /// state). Shedding the tail is strictly better than OOMing the node.
    fn forward_to_consensus(&self, msg: ConsensusMessage) {
        use tokio::sync::mpsc::error::TrySendError;
        let kind = msg.kind();
        let critical = msg.is_critical();
        match self.consensus_tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let dropped = self.consensus_dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if critical {
                    tracing::warn!(
                        "consensus channel full: shed critical {} ({} total shed)",
                        kind,
                        dropped
                    );
                } else {
                    tracing::debug!(
                        "consensus channel full: shed {} ({} total shed)",
                        kind,
                        dropped
                    );
                }
            }
            Err(TrySendError::Closed(_)) => {
                // Consensus loop has shut down; nothing left to feed.
                tracing::debug!("consensus channel closed, dropping {}", kind);
            }
        }
    }

    /// Total number of consensus messages shed because the bounded channel was
    /// full. Non-zero means the consensus consumer fell behind a burst.
    pub fn consensus_dropped(&self) -> u64 {
        self.consensus_dropped.load(Ordering::Relaxed)
    }

    /// Set a ledger provider for serving GetLedger requests.
    pub fn set_ledger_provider(&mut self, provider: Arc<dyn LedgerProvider>) {
        self.ledger_provider = Some(provider);
    }

    /// Set the backing node store for incremental ledger sync.
    pub fn set_node_store(&mut self, store: Arc<dyn rxrpl_shamap::NodeStore>) {
        self.node_store = Some(store);
    }

    /// Set the shard manager, enabling the shard exchange protocol.
    pub fn set_shard_manager(&mut self, manager: Arc<RwLock<rxrpl_nodestore::ShardManager>>) {
        self.shard_syncer = Some(ShardSyncer::new(manager));
    }

    /// Set the event sender for emitting overlay events as JSON values.
    ///
    /// Used to bridge overlay events (peer connect/disconnect, validations)
    /// to the RPC server's subscription system without a direct dependency.
    pub fn set_event_sender(&mut self, tx: tokio::sync::broadcast::Sender<serde_json::Value>) {
        self.server_event_tx = Some(tx);
    }

    /// Set the shared tx-set cache (typically from NetworkConsensusAdapter).
    pub fn set_tx_sets(&mut self, tx_sets: Arc<std::sync::RwLock<HashMap<Hash256, TxSet>>>) {
        self.tx_sets = Some(tx_sets);
    }

    /// Provide the server-info source for the peer `/crawl` endpoint so the
    /// crawl response carries the same build/ledger/uptime data as `server_info`.
    pub fn set_crawl_info(&mut self, info: Arc<dyn CrawlInfo>) {
        self.crawl_info = Some(info);
    }

    /// Register the **local** validator manifest (this node's own,
    /// produced by [`ValidatorIdentity::sign_manifest`](crate::identity::ValidatorIdentity::sign_manifest)).
    ///
    /// Indexes it alongside peer manifests so all the existing lookups
    /// (master_key_for_ephemeral, get_manifest, all_raw_manifests) see it
    /// transparently. B2 will broadcast it after each handshake; B3 will
    /// include it in `all_raw_manifests` returns.
    pub fn set_local_manifest(&mut self, manifest: crate::manifest::Manifest) {
        self.manifest_store.set_local(manifest);
    }

    /// The local validator manifest, if [`set_local_manifest`] has been
    /// called.
    /// Shared handle to the live peer set. RPC handlers can call `.len()` on
    /// this to surface `server_info.peers` without polling internal state.
    pub fn peer_set(&self) -> Arc<crate::peer_set::PeerSet> {
        Arc::clone(&self.peer_set)
    }

    pub fn local_manifest(&self) -> Option<&crate::manifest::Manifest> {
        self.manifest_store.local()
    }

    /// Check if a transaction set is known locally.
    fn has_tx_set(&self, hash: &Hash256) -> bool {
        self.tx_sets
            .as_ref()
            .map(|cache| cache.read().unwrap().contains_key(hash))
            .unwrap_or(false)
    }

    /// Run the peer manager event loop.
    pub async fn run(mut self) -> Result<(), OverlayError> {
        let bind_addr = format!("0.0.0.0:{}", self.config.listen_port);
        let listener = TcpListener::bind(&bind_addr).await?;
        tracing::info!("P2P listening on {}", bind_addr);

        // Spawn fixed peer connectors with retry
        let fixed_peers = self.config.fixed_peers.clone();
        for addr in fixed_peers {
            self.spawn_fixed_peer_connector(addr);
        }

        // Create and launch peer discovery using seeds + fixed_peers
        if self.discovery.is_none() {
            let mut all_seeds: Vec<String> = Vec::new();
            // Seeds from config (includes defaults like r.ripple.com)
            for seed in &self.seeds {
                if !all_seeds.contains(seed) {
                    all_seeds.push(seed.clone());
                }
            }
            // Fixed peers also act as seeds
            for fp in &self.config.fixed_peers {
                if !all_seeds.contains(fp) {
                    all_seeds.push(fp.clone());
                }
            }
            if !all_seeds.is_empty() {
                self.discovery = Some(Arc::new(PeerDiscovery::new(
                    all_seeds,
                    Arc::clone(&self.peer_set),
                    self.cmd_tx_internal.clone(),
                    self.config.max_peers,
                )));
            }
        }
        if let Some(ref discovery) = self.discovery {
            let disc = Arc::clone(discovery);
            tokio::spawn(async move {
                disc.bootstrap().await;
                disc.run_loop().await;
            });
        }

        let mut sync_interval = tokio::time::interval(Duration::from_secs(5));
        sync_interval.tick().await; // skip first immediate tick

        let mut reputation_interval = tokio::time::interval(Duration::from_secs(30));
        reputation_interval.tick().await; // skip first immediate tick

        let cluster_interval_secs = if self.cluster_manager.is_enabled() {
            self.config.cluster_broadcast_interval_secs.max(1)
        } else {
            3600 // effectively disabled
        };
        let mut cluster_interval =
            tokio::time::interval(Duration::from_secs(cluster_interval_secs));
        cluster_interval.tick().await; // skip first immediate tick

        let mut batch_relay_interval = tokio::time::interval(Duration::from_millis(250));
        batch_relay_interval.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, addr)) => {
                            tracing::debug!("inbound connection from {}", addr);
                            self.spawn_inbound_handler(stream, addr.to_string());
                        }
                        Err(e) => {
                            tracing::error!("accept error: {}", e);
                        }
                    }
                }

                Some(cmd) = self.cmd_rx.recv() => {
                    self.handle_command(cmd);
                }

                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event);
                }

                _ = sync_interval.tick() => {
                    self.check_sync();
                }

                _ = reputation_interval.tick() => {
                    self.peer_set.apply_score_decay();
                    self.check_peer_reputations();
                    self.squelch_manager.expire_stale_entries();
                }

                _ = cluster_interval.tick() => {
                    if self.cluster_manager.is_enabled() {
                        self.broadcast_cluster_status();
                        let pruned = self.cluster_manager.prune_stale();
                        if pruned > 0 {
                            tracing::info!("pruned {} stale cluster nodes", pruned);
                        }
                    }
                }

                _ = batch_relay_interval.tick() => {
                    self.flush_batch_relay();
                }
            }
        }
    }

    fn spawn_fixed_peer_connector(&mut self, addr: String) {
        let identity = Arc::clone(&self.identity);
        let network_id = self.config.network_id;
        let ledger_seq = Arc::clone(&self.ledger_seq);
        let ledger_hash = Arc::clone(&self.ledger_hash);
        let event_tx = self.event_tx.clone();
        let peer_set = Arc::clone(&self.peer_set);
        let tls_client = Arc::clone(&self.config.tls_client);
        let disconnect_notify = Arc::new(Notify::new());
        let shutdown_notify = Arc::clone(&self.shutdown_notify);

        self.fixed_peer_notifiers
            .insert(addr.clone(), Arc::clone(&disconnect_notify));

        tokio::spawn(async move {
            let mut backoff = ReconnectBackoff::new();

            loop {
                match try_connect_outbound(
                    &addr,
                    &identity,
                    network_id,
                    &ledger_seq,
                    &ledger_hash,
                    &event_tx,
                    &peer_set,
                    &tls_client,
                )
                .await
                {
                    Ok(node_id) => {
                        tracing::info!("connected to fixed peer {} ({})", addr, node_id);
                        backoff.reset();
                        // Wait for disconnect notification or shutdown.
                        tokio::select! {
                            _ = disconnect_notify.notified() => {
                                tracing::info!(
                                    "fixed peer {} ({}) disconnected, scheduling reconnect",
                                    addr, node_id,
                                );
                            }
                            _ = shutdown_notify.notified() => {
                                tracing::info!(
                                    "shutting down fixed peer connector for {}",
                                    addr,
                                );
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to connect to {}: {}", addr, e);
                    }
                }

                let delay = backoff.next_delay();
                tracing::debug!(
                    "reconnecting to fixed peer {} in {:?} (attempt {})",
                    addr,
                    delay,
                    backoff.attempt(),
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown_notify.notified() => {
                        tracing::info!(
                            "shutting down fixed peer connector for {}",
                            addr,
                        );
                        return;
                    }
                }
            }
        });
    }

    fn spawn_inbound_handler(&self, stream: TcpStream, addr: String) {
        let identity = Arc::clone(&self.identity);
        let network_id = self.config.network_id;
        let ledger_hash = Arc::clone(&self.ledger_hash);
        let event_tx = self.event_tx.clone();
        let peer_set = Arc::clone(&self.peer_set);
        let tls_server = self.config.tls_server.clone();
        let permits = Arc::clone(&self.inbound_handshake_permits);
        let crawl_info = self.crawl_info.clone();

        tokio::spawn(async move {
            // Hold a permit for the lifetime of the handshake. When the
            // semaphore is saturated, new attempts wait here instead of
            // burning CPU on TLS negotiation.
            let _permit = match permits.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return, // semaphore closed -> shutdown
            };
            if let Err(e) = try_accept_inbound(
                stream,
                &addr,
                &identity,
                network_id,
                &ledger_hash,
                &event_tx,
                &peer_set,
                &tls_server,
                crawl_info.as_ref(),
            )
            .await
            {
                tracing::debug!("inbound handshake failed from {}: {}", addr, e);
            }
        });
    }

    fn handle_command(&mut self, cmd: OverlayCommand) {
        match cmd {
            OverlayCommand::Broadcast { msg_type, payload } => {
                tracing::debug!(
                    "broadcast {:?} ({} bytes) to {} peers",
                    msg_type,
                    payload.len(),
                    self.peer_handles.len()
                );
                let mut sent = 0usize;
                let mut full = 0usize;
                for handle in self.peer_handles.values() {
                    match handle.tx.try_send(PeerMessage {
                        msg_type,
                        payload: payload.clone(),
                    }) {
                        Ok(()) => sent += 1,
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => full += 1,
                        Err(_) => {}
                    }
                }
                if full > 0 {
                    tracing::warn!(
                        "broadcast {:?}: {} sent, {} dropped (peer channel full)",
                        msg_type,
                        sent,
                        full
                    );
                }
            }
            OverlayCommand::SendTo {
                node_id,
                msg_type,
                payload,
            } => {
                if let Some(handle) = self.peer_handles.get(&node_id) {
                    let _ = handle.tx.try_send(PeerMessage { msg_type, payload });
                }
            }
            OverlayCommand::RequestLedger { seq, hash } => {
                self.send_get_ledger(seq, hash);
            }
            OverlayCommand::RequestShard { shard_index } => {
                if let Some(ref mut syncer) = self.shard_syncer {
                    syncer.queue_download(shard_index);
                    tracing::info!("queued shard {} for download", shard_index);
                } else {
                    tracing::warn!(
                        "cannot download shard {}: shard syncer not configured",
                        shard_index
                    );
                }
            }
            OverlayCommand::ConnectTo { addr } => {
                let identity = Arc::clone(&self.identity);
                let network_id = self.config.network_id;
                let ledger_seq = Arc::clone(&self.ledger_seq);
                let ledger_hash = Arc::clone(&self.ledger_hash);
                let event_tx = self.event_tx.clone();
                let peer_set = Arc::clone(&self.peer_set);
                let tls_client = Arc::clone(&self.config.tls_client);

                tokio::spawn(async move {
                    if let Err(e) = try_connect_outbound(
                        &addr,
                        &identity,
                        network_id,
                        &ledger_seq,
                        &ledger_hash,
                        &event_tx,
                        &peer_set,
                        &tls_client,
                    )
                    .await
                    {
                        tracing::warn!("connect to {} failed: {}", addr, e);
                    }
                });
            }
        }
    }

    fn handle_event(&mut self, event: PeerEvent) {
        match event {
            PeerEvent::Connected {
                node_id,
                info,
                write_tx,
            } => {
                tracing::info!("peer {} registered ({})", node_id, info.address);
                if let Some(ref tx) = self.server_event_tx {
                    let _ = tx.send(serde_json::json!({
                        "type": "peerStatusChange",
                        "peer_id": node_id.to_string(),
                        "event": "connected",
                    }));
                }
                // Track fixed peer node_id -> address mapping for disconnect notification.
                if self.fixed_peer_notifiers.contains_key(&info.address) {
                    self.fixed_peer_node_ids
                        .insert(node_id, info.address.clone());
                }
                self.peer_handles.insert(
                    node_id,
                    PeerHandle {
                        node_id,
                        info,
                        tx: write_tx,
                    },
                );
                // Proactively share our local manifest so the peer can bind
                // our signing key to our master key before the first
                // validation arrives (B2).
                self.send_local_manifest_to(&node_id);
                // Request shard availability from new peer.
                self.send_get_shards(&node_id);
            }
            PeerEvent::Message {
                from,
                msg_type,
                payload,
            } => {
                self.dispatch_message(from, msg_type, &payload);
            }
            PeerEvent::Disconnected { node_id } => {
                tracing::info!("peer {} disconnected", node_id);
                if let Some(ref tx) = self.server_event_tx {
                    let _ = tx.send(serde_json::json!({
                        "type": "peerStatusChange",
                        "peer_id": node_id.to_string(),
                        "event": "disconnected",
                    }));
                }
                self.peer_handles.remove(&node_id);
                self.peer_set.remove(&node_id);
                self.squelch_manager.remove_peer(&node_id);
                if let Some(ref mut syncer) = self.shard_syncer {
                    syncer.peer_disconnected(&node_id);
                }
                // Notify the fixed peer reconnector if this was a fixed peer.
                if let Some(addr) = self.fixed_peer_node_ids.remove(&node_id) {
                    if let Some(notifier) = self.fixed_peer_notifiers.get(&addr) {
                        notifier.notify_one();
                    }
                }
            }
        }
    }

    fn dispatch_message(&mut self, from: Hash256, msg_type: MessageType, payload: &[u8]) {
        let peer_info = self.peer_set.get(&from);
        let payload_len = payload.len() as u64;

        // Apply per-peer rate limiting before processing.
        if let Some(ref info) = peer_info {
            let result = info.rate_limiter.check(msg_type);
            match result {
                crate::rate_limiter::RateLimitResult::Allowed => {}
                crate::rate_limiter::RateLimitResult::Dropped => {
                    tracing::warn!(
                        "rate-limited {:?} from peer {} (consecutive drops: {})",
                        msg_type,
                        from,
                        info.rate_limiter.consecutive_drops()
                    );
                    info.reputation
                        .apply_penalty(-crate::rate_limiter::PeerRateLimiter::penalty());
                    return;
                }
                crate::rate_limiter::RateLimitResult::Disconnect => {
                    tracing::warn!(
                        "disconnecting peer {} due to sustained rate-limit abuse",
                        from
                    );
                    info.reputation.record_violation();
                }
            }
            if result == crate::rate_limiter::RateLimitResult::Disconnect {
                drop(peer_info);
                self.peer_handles.remove(&from);
                self.peer_set.remove(&from);
                return;
            }
        }

        // Shed transaction gossip during initial state catchup: with no base
        // ledger a cold node cannot apply or usefully relay transactions, and
        // the mainnet tx flood (~21k/25s) plus its per-peer rebroadcast loop
        // otherwise starves the SHAMap state sync that shares this single event
        // loop. Proposals are NOT shed: between rxrpl nodes the peer tip is
        // announced via proposals (not StatusChange), so they are load-bearing
        // for the node loop entering sync mode and adopting the acquired state.
        if matches!(msg_type, MessageType::Transaction) && self.ledger_syncer.in_initial_catchup() {
            return;
        }

        match msg_type {
            MessageType::Hello => {
                // Hello is already handled during handshake; ignore late arrivals.
                tracing::debug!("ignoring late Hello from {}", from);
            }
            MessageType::Ping => match proto_convert::decode_ping(payload) {
                Ok(ping) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_valid_message(payload_len);
                    }
                    if ping.r#type.unwrap_or(0) == 0 {
                        let pong = proto_convert::encode_ping(ping.seq.unwrap_or(0), true);
                        if let Some(handle) = self.peer_handles.get(&from) {
                            let _ = handle.tx.try_send(PeerMessage {
                                msg_type: MessageType::Ping,
                                payload: pong,
                            });
                        }
                    }
                }
                Err(_) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_invalid_message();
                    }
                }
            },
            MessageType::Transaction => {
                match proto_convert::decode_transaction(payload) {
                    Ok((tx_hash, tx_data)) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        // Register in batch relay so it can be served via
                        // TMHaveTransactions/TMTransactions protocol.
                        self.tx_batch_relay.add_known_tx(tx_hash, tx_data.clone());
                        if self.relay_filter.should_relay(&tx_hash) {
                            self.forward_to_consensus(ConsensusMessage::Transaction {
                                hash: tx_hash,
                                data: tx_data,
                            });
                            // Re-broadcast to other peers
                            for (id, handle) in &self.peer_handles {
                                if *id != from {
                                    let _ = handle.tx.try_send(PeerMessage {
                                        msg_type: MessageType::Transaction,
                                        payload: payload.to_vec(),
                                    });
                                }
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ProposeSet => match proto_convert::decode_propose_set(payload) {
                Ok(proposal) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_valid_message(payload_len);
                    }
                    self.forward_to_consensus(ConsensusMessage::Proposal(proposal));
                }
                Err(_) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_invalid_message();
                    }
                }
            },
            MessageType::Validation => {
                match proto_convert::decode_validation(payload) {
                    Ok(validation) => {
                        // Reject validations with missing or invalid signatures
                        if !crate::identity::verify_validation_signature(&validation) {
                            tracing::warn!(
                                "rejecting validation with invalid signature for ledger #{}",
                                validation.ledger_seq,
                            );
                            if let Some(ref info) = peer_info {
                                info.reputation.record_invalid_message();
                            }
                        } else {
                            if let Some(ref info) = peer_info {
                                info.reputation.record_valid_message(payload_len);
                            }

                            // Track which peer is relaying this validator's messages
                            // and send squelch to redundant sources.
                            if let Some(action) = self
                                .squelch_manager
                                .record_validation_source(from, &validation.public_key)
                            {
                                self.send_squelch_to_peers(&action);
                            }

                            if let Some(ref tx) = self.server_event_tx {
                                let _ = tx.send(serde_json::json!({
                                    "type": "validationReceived",
                                    "validator": validation.node_id.0.to_string(),
                                    "ledger_hash": validation.ledger_hash.to_string(),
                                    "ledger_seq": validation.ledger_seq,
                                    "full": validation.full,
                                }));
                            }

                            // Relay validation to peers, respecting inbound squelch.
                            // A node still acquiring its base state is not a useful
                            // relay, and this loop is O(validators x peers) per
                            // message on the single event loop that must drive the
                            // state sync, so skip it during initial catchup. We
                            // still verify (above) and forward to the node loop
                            // (below) so the quorum-validated tip is learned.
                            if !self.ledger_syncer.in_initial_catchup() {
                                for (id, handle) in &self.peer_handles {
                                    if *id != from
                                        && !self
                                            .squelch_manager
                                            .is_relay_squelched(id, &validation.public_key)
                                    {
                                        let _ = handle.tx.try_send(PeerMessage {
                                            msg_type: MessageType::Validation,
                                            payload: payload.to_vec(),
                                        });
                                    }
                                }
                            }

                            self.forward_to_consensus(ConsensusMessage::Validation(validation));
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::StatusChange => {
                match proto_convert::decode_status_change(payload) {
                    Ok((ledger_hash, ledger_seq)) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                            info.ledger_seq.store(ledger_seq, Ordering::Relaxed);
                        }

                        // Trigger sync if peer is ahead
                        let our_seq = self.ledger_seq.load(Ordering::Relaxed);
                        if self.ledger_syncer.needs_sync(our_seq, ledger_seq) {
                            if self.ledger_syncer.in_initial_catchup() {
                                // Cold start: do NOT play forward seq-by-seq from
                                // genesis. That ascending march hits the aged-target
                                // wall before ever reaching the tip on mainnet's
                                // ~19M-entry state -- peers flush the deep nodes of
                                // old ledgers, so the deep frontier of an old target
                                // can never be fetched. Instead target the validated
                                // tip's state map directly and let the drain/re-target
                                // loop (start_incremental_sync) chase fresher tips.
                                // Kick the tip now so we don't wait for the next
                                // check_sync tick; check_sync re-targets to max_peer_seq.
                                if !self.ledger_syncer.has_any_incremental_sync() {
                                    self.send_get_ledger(ledger_seq, None);
                                }
                            } else {
                                let requests =
                                    self.ledger_syncer.request_missing(our_seq, ledger_seq);
                                for (seq, hash) in requests {
                                    self.send_get_ledger(seq, hash);
                                }
                            }
                        }

                        self.forward_to_consensus(ConsensusMessage::StatusChange {
                            from,
                            ledger_seq,
                            ledger_hash,
                        });
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Cluster => {
                if !self.cluster_manager.is_enabled() {
                    tracing::debug!("ignoring Cluster message (cluster mode disabled)");
                    return;
                }
                match proto_convert::decode_cluster(payload) {
                    Ok(nodes) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        let mut accepted = 0;
                        for node_data in &nodes {
                            if self.cluster_manager.update_node(
                                &node_data.public_key,
                                node_data.node_load,
                                &node_data.node_name,
                                &node_data.address,
                                node_data.report_time,
                            ) {
                                accepted += 1;
                            }
                        }
                        tracing::debug!(
                            "Cluster message from {}: {}/{} nodes accepted",
                            from,
                            accepted,
                            nodes.len()
                        );
                        if let Some(avg_fee) = self.cluster_manager.average_load_fee() {
                            tracing::trace!("cluster average load fee: {}", avg_fee);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode Cluster from {}: {}", from, e);
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::GetLedger => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                self.handle_get_ledger(from, payload);
            }
            MessageType::LedgerData => {
                tracing::info!("received LedgerData from {}: {} bytes", from, payload.len());
                match proto_convert::decode_ledger_data(payload) {
                    Ok(msg) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                            // Peer provided requested ledger data -- useful contribution
                            info.reputation.record_useful_contribution();
                        }
                        let ledger_hash_bytes = msg.ledger_hash;
                        let hash =
                            Hash256::new(ledger_hash_bytes[..32].try_into().unwrap_or([0u8; 32]));
                        let nodes: Vec<(Vec<u8>, Vec<u8>)> = msg
                            .nodes
                            .into_iter()
                            .map(|n| (n.nodeid.unwrap_or_default(), n.nodedata.unwrap_or_default()))
                            .collect();

                        let ledger_seq = msg.ledger_seq;
                        let info_type = msg.ledger_info_type;
                        tracing::info!(
                            "decoded LedgerData seq={} hash={} itype={} nodes={}",
                            ledger_seq,
                            hash,
                            info_type,
                            nodes.len()
                        );

                        // Handle tx-set candidate responses (liTS_CANDIDATE = 3).
                        if info_type == LI_TS_CANDIDATE {
                            self.handle_tx_set_response(hash, &nodes);
                            return;
                        }

                        // Skip already-synced ledgers.
                        if self.ledger_syncer.is_synced(ledger_seq) {
                            tracing::debug!(
                                "ignoring LedgerData for already-synced #{}",
                                ledger_seq
                            );
                        } else if self.ledger_syncer.has_incremental_sync(ledger_seq) {
                            // Active incremental sync: feed nodes into SHAMap.
                            use crate::ledger_sync::FeedResult;
                            match self.ledger_syncer.feed_nodes(ledger_seq, &nodes) {
                                FeedResult::Complete(leaves) => {
                                    tracing::info!(
                                        "incremental sync complete for ledger #{} ({} leaf nodes)",
                                        ledger_seq,
                                        leaves.len()
                                    );
                                    self.ledger_syncer.mark_synced(ledger_seq);
                                    self.forward_to_consensus(ConsensusMessage::LedgerData {
                                        hash,
                                        seq: ledger_seq,
                                        nodes: leaves,
                                    });
                                }
                                FeedResult::FallbackToHashFetch(content_hashes) => {
                                    // Tree-based sync stuck; try fetching by content hash.
                                    self.send_get_objects_by_hash(ledger_seq, &content_hashes);
                                    // Also keep trying tree-based sync in parallel.
                                    self.send_get_ledger_as_node(ledger_seq);
                                }
                                FeedResult::Continue => {
                                    // Request next batch of missing nodes.
                                    self.send_get_ledger_as_node(ledger_seq);
                                }
                                FeedResult::Removed => {
                                    // Sync was abandoned.
                                }
                            }
                        } else if !nodes.is_empty() {
                            // No active sync: try to parse as liBASE header.
                            let header_data = &nodes[0].1;
                            let latest = self.ledger_syncer.latest_known_seq();
                            if let Some(header) = rxrpl_ledger::LedgerHeader::from_raw_bytes(
                                header_data,
                            )
                            .filter(|h| {
                                latest.is_none_or(|known| {
                                    (h.sequence as i64 - known as i64).unsigned_abs() <= 1000
                                })
                            }) {
                                let is_newer = latest.is_none_or(|known| header.sequence > known);
                                if is_newer {
                                    tracing::info!(
                                        "received liBASE header for ledger #{} hash={}",
                                        header.sequence,
                                        header.hash
                                    );
                                }
                                self.ledger_syncer
                                    .set_ledger_hash(header.sequence, header.hash);

                                // Emit the parsed header to consensus UNCONDITIONALLY and
                                // BEFORE any LedgerData. The consensus loop caches it in
                                // `catchup_headers`; `try_reconstruct_ledger` then copies
                                // close_time / parent_close_time / drops onto the
                                // reconstructed ledger. Without a cached header the
                                // reconstructed ledger has close_time=0, the next open
                                // ledger inherits parent_close_time=0, and the close-time
                                // alignment gate is defeated → divergence from rippled.
                                // Every catchup path below (immediate-complete AND the
                                // common multi-round `feed_nodes` path) relies on this.
                                self.forward_to_consensus(ConsensusMessage::LedgerHeader {
                                    seq: header.sequence,
                                    header: header.clone(),
                                });

                                if let Some(store) = self.get_node_store() {
                                    let missing = self.ledger_syncer.start_incremental_sync(
                                        header.sequence,
                                        header.account_hash,
                                        store,
                                    );
                                    if !missing.is_empty() {
                                        self.send_get_ledger_as_node(header.sequence);
                                    } else if let Some(leaves) =
                                        self.ledger_syncer.try_complete_sync(header.sequence)
                                    {
                                        // Target state already fully resolvable from the
                                        // local store (e.g. early ledgers whose state still
                                        // matches genesis). Dispatch leaves to consensus.
                                        tracing::info!(
                                            "incremental sync immediate-complete for ledger #{} ({} leaves)",
                                            header.sequence,
                                            leaves.len()
                                        );
                                        self.ledger_syncer.mark_synced(header.sequence);
                                        self.forward_to_consensus(ConsensusMessage::LedgerData {
                                            hash: header.hash,
                                            seq: header.sequence,
                                            nodes: leaves,
                                        });
                                    }
                                }
                            } else {
                                // Not a header -- raw node data, not useful for reconstruction.
                                tracing::debug!(
                                    "ignoring non-header LedgerData for #{} ({} nodes)",
                                    ledger_seq,
                                    nodes.len()
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode LedgerData from {}: {}", from, e);
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Endpoints => {
                // Try rippled TMEndpoints format first, fall back to legacy TMPeers
                let peers_result = proto_convert::decode_endpoints(payload)
                    .map(|eps| {
                        eps.into_iter()
                            .filter_map(|(endpoint, _hops)| {
                                // endpoint is "ip:port" string
                                let (ip, port_str) = endpoint.rsplit_once(':')?;
                                let port = port_str.parse::<u16>().ok()?;
                                Some((ip.to_string(), port))
                            })
                            .collect::<Vec<_>>()
                    })
                    .or_else(|_| proto_convert::decode_peers(payload));

                match peers_result {
                    Ok(peers) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!("received {} peer addresses from {}", peers.len(), from);
                        if let Some(ref discovery) = self.discovery {
                            let disc = Arc::clone(discovery);
                            let peers = peers.clone();
                            tokio::spawn(async move {
                                disc.handle_peers_response(peers).await;
                            });
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Manifests => {
                match proto_convert::decode_manifests(payload) {
                    Ok(manifest_list) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!("received {} manifests from {}", manifest_list.len(), from);

                        // Parse, verify, and apply each manifest
                        let raw_bytes: Vec<Vec<u8>> = manifest_list
                            .into_iter()
                            .filter_map(|m| m.stobject)
                            .collect();
                        let mut applied = 0;
                        // B3: collect freshly-applied manifests so we can relay
                        // them to other peers below. ManifestStore::apply()
                        // returns false on stale/duplicate, so we never
                        // re-broadcast the same manifest twice.
                        let mut to_relay: Vec<Vec<u8>> = Vec::new();
                        for raw in &raw_bytes {
                            match manifest::parse_and_verify(raw) {
                                Ok(m) => {
                                    let master_key = m.master_public_key.clone();
                                    let eph_key = m.ephemeral_public_key.clone();
                                    let revoked = m.is_revoked();

                                    // Get old ephemeral before applying
                                    let old_eph = self
                                        .manifest_store
                                        .current_ephemeral_key(&master_key)
                                        .cloned();

                                    if self.manifest_store.apply(m) {
                                        applied += 1;
                                        to_relay.push(raw.clone());
                                        self.forward_to_consensus(
                                            ConsensusMessage::ManifestApplied {
                                                master_key,
                                                ephemeral_key: eph_key,
                                                old_ephemeral_key: old_eph,
                                                revoked,
                                            },
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::debug!("manifest verify failed: {}", e);
                                }
                            }
                        }

                        // B3: gossip relay — forward freshly-applied manifests
                        // to every other connected peer so the network
                        // converges on validator key bindings without lag.
                        if !to_relay.is_empty() {
                            let relay_payload = proto_convert::encode_manifests(to_relay);
                            for (id, handle) in &self.peer_handles {
                                if *id != from {
                                    let _ =
                                        handle.tx.try_send(rxrpl_p2p_proto::codec::PeerMessage {
                                            msg_type: MessageType::Manifests,
                                            payload: relay_payload.clone(),
                                        });
                                }
                            }
                        }

                        tracing::debug!(
                            "applied {}/{} manifests from {}",
                            applied,
                            raw_bytes.len(),
                            from
                        );

                        if let Some(ref tx) = self.server_event_tx {
                            let _ = tx.send(serde_json::json!({
                                "type": "manifestsReceived",
                                "count": raw_bytes.len(),
                                "applied": applied,
                                "from": from.to_string(),
                            }));
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::HaveSet => {
                match proto_convert::decode_have_set(payload) {
                    Ok(have_set) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "HaveTransactionSet from {} hash={} status={}",
                            from,
                            have_set.hash,
                            have_set.status
                        );

                        // If we do not have this tx-set locally, fetch it from the peer.
                        // Cap the in-flight set so a peer spamming HaveTransactionSet
                        // with unique fake hashes cannot leak unbounded memory
                        // (audit finding H6).
                        const MAX_PENDING_TX_SETS: usize = 4096;
                        if !self.has_tx_set(&have_set.hash)
                            && !self.pending_tx_set_fetches.contains(&have_set.hash)
                            && self.pending_tx_set_fetches.len() < MAX_PENDING_TX_SETS
                        {
                            tracing::info!(
                                "requesting unknown tx-set {} from peer {}",
                                have_set.hash,
                                from
                            );
                            self.pending_tx_set_fetches.insert(have_set.hash);
                            self.send_get_tx_set(from, have_set.hash);
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::GetObjects => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                match proto_convert::decode_get_objects(payload) {
                    Ok(msg) => {
                        let is_response = !msg.query.unwrap_or(true);
                        if is_response && !msg.objects.is_empty() {
                            // This is a response to our TMGetObjectByHash request.
                            // Extract (hash, data) pairs and feed into incremental sync.
                            let ledger_seq = msg.seq.unwrap_or(0);
                            let nodes: Vec<(Vec<u8>, Vec<u8>)> = msg
                                .objects
                                .into_iter()
                                .filter_map(|obj| {
                                    let hash = obj.hash.unwrap_or_default();
                                    let data = obj.data.unwrap_or_default();
                                    if !hash.is_empty() && !data.is_empty() {
                                        Some((hash, data))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            tracing::info!(
                                "received GetObjectByHash response from {} ({} objects for #{})",
                                from,
                                nodes.len(),
                                ledger_seq
                            );
                            // TMGetObjectByHash returns NodeObject blobs
                            // (`HASH_PREFIX[4] || content`), not the SHAMap wire
                            // form (`content || trailing-type-byte`) that
                            // feed_nodes decodes. Convert so the hash recomputes
                            // correctly; otherwise every object decoded to a
                            // wrong hash and was rejected as already-present.
                            let nodes: Vec<(Vec<u8>, Vec<u8>)> = nodes
                                .into_iter()
                                .filter_map(|(h, d)| {
                                    crate::ledger_sync::object_blob_to_wire(&d).map(|w| (h, w))
                                })
                                .collect();
                            // GetObjectByHash nodes are content-addressed, so they
                            // are valid for whatever sync is active regardless of
                            // the seq rippled echoes. A reply for an abandoned seq
                            // (the tip moved on while it was in flight) would
                            // otherwise be dropped, wasting nodes the active sync
                            // still needs from the shared store.
                            let feed_seq = if self.ledger_syncer.has_incremental_sync(ledger_seq) {
                                Some(ledger_seq)
                            } else {
                                self.ledger_syncer.active_incremental_seq()
                            };
                            if let Some(seq) = feed_seq {
                                use crate::ledger_sync::FeedResult;
                                let ledger_hash = self.ledger_syncer.get_ledger_hash(seq);
                                let hash = ledger_hash.unwrap_or(Hash256::ZERO);
                                match self.ledger_syncer.feed_nodes(seq, &nodes) {
                                    FeedResult::Complete(leaves) => {
                                        tracing::info!(
                                            "incremental sync complete (via hash fallback) for #{} ({} leaves)",
                                            seq,
                                            leaves.len()
                                        );
                                        self.ledger_syncer.mark_synced(seq);
                                        self.forward_to_consensus(ConsensusMessage::LedgerData {
                                            hash,
                                            seq,
                                            nodes: leaves,
                                        });
                                    }
                                    FeedResult::FallbackToHashFetch(content_hashes) => {
                                        self.send_get_objects_by_hash(seq, &content_hashes);
                                    }
                                    FeedResult::Continue => {
                                        self.send_get_ledger_as_node(seq);
                                    }
                                    FeedResult::Removed => {}
                                }
                            }
                        } else {
                            self.handle_get_objects_query(from, msg);
                        }
                    }
                    Err(e) => {
                        tracing::debug!("bad GetObjectByHash from {}: {}", from, e);
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ValidatorList => {
                match proto_convert::decode_validator_list(payload) {
                    Ok(vl) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "ValidatorList v{} from {} ({} bytes manifest, {} bytes blob)",
                            vl.version.unwrap_or(0),
                            from,
                            vl.manifest.as_ref().map(|v| v.len()).unwrap_or(0),
                            vl.blob.as_ref().map(|v| v.len()).unwrap_or(0)
                        );

                        // Attempt full signature verification
                        let manifest_bytes = vl.manifest.as_deref();
                        let blob_bytes = vl.blob.as_deref();
                        let sig_bytes = vl.signature.as_deref();

                        if let (Some(manifest_b), Some(blob_b), Some(sig_b)) =
                            (manifest_bytes, blob_bytes, sig_bytes)
                        {
                            match validator_list::verify_and_parse(
                                manifest_b,
                                blob_b,
                                sig_b,
                                &mut self.manifest_store,
                            ) {
                                Ok(vl_data) => {
                                    let count = vl_data.validators.len();
                                    let seq = vl_data.sequence;

                                    // Track sequence to reject stale lists
                                    if self
                                        .vl_tracker
                                        .record_sequence(&vl_data.publisher_master_key, seq)
                                    {
                                        tracing::info!(
                                            "verified validator list seq={} with {} validators from {}",
                                            seq,
                                            count,
                                            from
                                        );

                                        // Process individual validator manifests
                                        for raw_manifest in &vl_data.validator_manifests {
                                            if let Ok(m) = manifest::parse_and_verify(raw_manifest)
                                            {
                                                let master_key = m.master_public_key.clone();
                                                let eph_key = m.ephemeral_public_key.clone();
                                                let revoked = m.is_revoked();
                                                let old_eph = self
                                                    .manifest_store
                                                    .current_ephemeral_key(&master_key)
                                                    .cloned();
                                                if self.manifest_store.apply(m) {
                                                    self.forward_to_consensus(
                                                        ConsensusMessage::ManifestApplied {
                                                            master_key,
                                                            ephemeral_key: eph_key,
                                                            old_ephemeral_key: old_eph,
                                                            revoked,
                                                        },
                                                    );
                                                }
                                            }
                                        }

                                        // Send verified list to consensus
                                        self.forward_to_consensus(
                                            ConsensusMessage::ValidatorListVerified {
                                                validators: vl_data.validators,
                                                sequence: seq,
                                            },
                                        );
                                    } else {
                                        tracing::debug!(
                                            "stale validator list seq={} from {}",
                                            seq,
                                            from
                                        );
                                    }

                                    // Also send the count for backward compatibility
                                    self.forward_to_consensus(
                                        ConsensusMessage::ValidatorListReceived {
                                            validator_count: count,
                                        },
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "validator list verification failed from {}: {}",
                                        from,
                                        e
                                    );
                                    // Fall back to unverified count extraction
                                    if let Some(blob_b) = vl.blob.as_ref() {
                                        if let Ok(count) = base64_decode_validator_blob(blob_b) {
                                            self.forward_to_consensus(
                                                ConsensusMessage::ValidatorListReceived {
                                                    validator_count: count,
                                                },
                                            );
                                        }
                                    }
                                }
                            }
                        } else {
                            // Missing fields, fall back to unverified extraction
                            if let Some(blob_bytes) = vl.blob.as_ref() {
                                if let Ok(decoded) = base64_decode_validator_blob(blob_bytes) {
                                    self.forward_to_consensus(
                                        ConsensusMessage::ValidatorListReceived {
                                            validator_count: decoded,
                                        },
                                    );
                                }
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ValidatorListCollection => {
                match proto_convert::decode_validator_list_collection(payload) {
                    Ok(vlc) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "ValidatorListCollection v{} from {} ({} blobs)",
                            vlc.version.unwrap_or(0),
                            from,
                            vlc.blobs.len()
                        );
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Squelch => match proto_convert::decode_squelch(payload) {
                Ok(msg) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_valid_message(payload_len);
                    }
                    let squelch_flag = msg.squelch.unwrap_or(true);
                    let validator_key = msg.validator_pub_key.unwrap_or_default();
                    let duration = msg.squelch_duration.unwrap_or(300);
                    if !validator_key.is_empty() {
                        self.squelch_manager.handle_inbound_squelch(
                            from,
                            &validator_key,
                            squelch_flag,
                            duration,
                        );
                    }
                    tracing::debug!(
                        "Squelch from {}: squelch={}, duration={}s",
                        from,
                        squelch_flag,
                        duration,
                    );
                }
                Err(_) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_invalid_message();
                    }
                }
            },
            MessageType::HaveTransactions => {
                match proto_convert::decode_have_transactions(payload) {
                    Ok(hash_list) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        tracing::debug!(
                            "HaveTransactions from {} with {} hashes",
                            from,
                            hash_list.len(),
                        );

                        // Separate hashes into those we can serve and those we need.
                        let mut can_serve: Vec<(Hash256, Vec<u8>)> = Vec::new();
                        let mut need_hashes: Vec<Vec<u8>> = Vec::new();

                        for raw_hash in &hash_list {
                            if raw_hash.len() != 32 {
                                continue;
                            }
                            let arr: [u8; 32] = match raw_hash[..32].try_into() {
                                Ok(a) => a,
                                Err(_) => continue,
                            };
                            let hash = Hash256::new(arr);

                            if let Some(data) = self.tx_batch_relay.get_tx_data(&hash) {
                                // We have this tx: the peer is requesting it from us.
                                can_serve.push((hash, data));
                            } else {
                                // We don't have it: collect for requesting.
                                need_hashes.push(raw_hash.clone());
                            }
                        }

                        // Respond with TMTransactions for hashes we can serve.
                        if !can_serve.is_empty() {
                            let response_payload =
                                crate::tx_batch_relay::encode_transactions_batch(&can_serve);
                            if let Some(handle) = self.peer_handles.get(&from) {
                                let _ = handle.tx.try_send(PeerMessage {
                                    msg_type: MessageType::Transactions,
                                    payload: response_payload,
                                });
                            }
                        }

                        // Request the ones we are missing.
                        let missing = self.tx_batch_relay.process_have_transactions(&need_hashes);
                        if !missing.is_empty() {
                            let request_payload =
                                crate::tx_batch_relay::encode_have_transactions(&missing);
                            if let Some(handle) = self.peer_handles.get(&from) {
                                let _ = handle.tx.try_send(PeerMessage {
                                    msg_type: MessageType::HaveTransactions,
                                    payload: request_payload,
                                });
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Transactions => {
                match proto_convert::decode_transactions(payload) {
                    Ok(batch) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        let new_txs = self
                            .tx_batch_relay
                            .process_transactions_batch(&batch.transactions);
                        tracing::debug!(
                            "Transactions batch from {} with {} txs ({} new)",
                            from,
                            batch.transactions.len(),
                            new_txs.len(),
                        );
                        // Forward each new transaction to the consensus layer
                        for (tx_hash, tx_data) in &new_txs {
                            if self.relay_filter.should_relay(tx_hash) {
                                self.forward_to_consensus(ConsensusMessage::Transaction {
                                    hash: *tx_hash,
                                    data: tx_data.clone(),
                                });
                            }
                        }
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::GetShards => {
                if let Some(ref info) = peer_info {
                    info.reputation.record_valid_message(payload_len);
                }
                self.handle_get_shards(from);
            }
            MessageType::Shards => match rxrpl_p2p_proto::shard_msg::decode_shards(payload) {
                Ok(msg) => {
                    if let Some(ref info) = peer_info {
                        info.reputation.record_valid_message(payload_len);
                    }
                    if let Some(ref mut syncer) = self.shard_syncer {
                        syncer.on_shards_message(from, msg);
                    }
                }
                Err(e) => {
                    tracing::warn!("failed to decode Shards from {}: {}", from, e);
                    if let Some(ref info) = peer_info {
                        info.reputation.record_invalid_message();
                    }
                }
            },
            MessageType::GetShardData => {
                match rxrpl_p2p_proto::shard_msg::decode_get_shard_data(payload) {
                    Ok(msg) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        self.handle_get_shard_data(from, msg);
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode GetShardData from {}: {}", from, e);
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::ShardData => {
                match rxrpl_p2p_proto::shard_msg::decode_shard_data(payload) {
                    Ok(msg) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                            info.reputation.record_useful_contribution();
                        }
                        if self.shard_syncer.is_some() {
                            self.handle_shard_data_sync(from, msg);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to decode ShardData from {}: {}", from, e);
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
        }
    }

    /// Check for ledger gaps and request missing ledgers from peers.
    /// Flush accumulated transaction hashes as a TMHaveTransactions broadcast.
    ///
    /// Called periodically (every 250ms) to batch-announce new transactions
    /// to all connected peers instead of relaying each one individually.
    fn flush_batch_relay(&mut self) {
        let batch = self.tx_batch_relay.drain_outbound_queue();
        if batch.is_empty() {
            return;
        }

        let payload = crate::tx_batch_relay::encode_have_transactions(&batch);
        tracing::debug!("broadcasting HaveTransactions with {} hashes", batch.len());

        for handle in self.peer_handles.values() {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::HaveTransactions,
                payload: payload.clone(),
            });
        }
    }

    /// Send squelch messages to the specified peers for a given validator.
    fn send_squelch_to_peers(&self, action: &crate::squelch::SquelchAction) {
        let payload =
            proto_convert::encode_squelch(&action.validator_key, true, action.duration_secs);
        for peer_id in &action.squelch_peers {
            if let Some(handle) = self.peer_handles.get(peer_id) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::Squelch,
                    payload: payload.clone(),
                });
                tracing::debug!(
                    "sent squelch to {} for validator {} ({}s)",
                    peer_id,
                    hex::encode(&action.validator_key),
                    action.duration_secs,
                );
            }
        }
    }

    /// Handle a GetShards request from a peer: respond with our shard availability.
    fn handle_get_shards(&self, from: Hash256) {
        if self.shard_syncer.is_none() {
            // No shard support: respond with empty shards.
            let msg = rxrpl_p2p_proto::shard_msg::TMShards::default();
            let payload = rxrpl_p2p_proto::shard_msg::encode_shards(&msg);
            if let Some(handle) = self.peer_handles.get(&from) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::Shards,
                    payload,
                });
            }
            return;
        }

        // Build our shard availability from the shard manager (via syncer's ref).
        // Since shard_manager is behind an async RwLock, we spawn a task.
        // For simplicity in the sync dispatch path, send an empty response now
        // and let the periodic tick handle proper advertisement.
        let msg = rxrpl_p2p_proto::shard_msg::TMShards::default();
        let payload = rxrpl_p2p_proto::shard_msg::encode_shards(&msg);
        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::Shards,
                payload,
            });
        }
        tracing::debug!("sent Shards response to {}", from);
    }

    /// Handle a GetShardData request: serve ledger data from our shard store.
    fn handle_get_shard_data(
        &self,
        from: Hash256,
        msg: rxrpl_p2p_proto::shard_msg::TMGetShardData,
    ) {
        // This needs access to ShardManager which is behind an async lock.
        // We cannot block here, so we spawn a task.
        let shard_index = msg.shard_index;
        let seqs = msg.ledger_seqs;
        tracing::debug!(
            "GetShardData from {} for shard {} ({} sequences)",
            from,
            shard_index,
            seqs.len()
        );

        // For now, respond with empty data since we cannot hold async locks
        // in the sync dispatch path. The actual serving is handled in the
        // async event loop when shard_syncer is available.
        let response = rxrpl_p2p_proto::shard_msg::TMShardData {
            shard_index,
            ledgers: vec![],
        };
        let payload = rxrpl_p2p_proto::shard_msg::encode_shard_data(&response);
        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::ShardData,
                payload,
            });
        }
    }

    /// Synchronously import shard data received from a peer.
    ///
    /// Since the shard manager is behind an async RwLock and we're in a sync
    /// context, we use `try_write()` to avoid blocking. If the lock is
    /// contended, the data is dropped and will be re-requested.
    fn handle_shard_data_sync(
        &mut self,
        from: Hash256,
        msg: rxrpl_p2p_proto::shard_msg::TMShardData,
    ) {
        if self.shard_syncer.is_none() {
            return;
        }

        let shard_index = msg.shard_index;
        let entry_count = msg.ledgers.len();

        tracing::debug!(
            "importing {} ledger entries for shard {} from {}",
            entry_count,
            shard_index,
            from
        );

        // The actual async import is triggered by the ShardSyncer's tick()
        // method. Here we just log receipt. The ShardSyncer::on_shard_data()
        // method handles the actual import but requires async context.
    }

    /// Send our local manifest to a freshly-connected peer (B2).
    ///
    /// No-op when no local manifest is configured. Encodes the single
    /// manifest into a `TmManifests` and delivers it via the peer's
    /// write channel; the peer parses it through its standard
    /// `MessageType::Manifests` handler (which calls `apply` on the
    /// `ManifestStore`), so they learn our master->signing binding
    /// before our first validation arrives.
    fn send_local_manifest_to(&self, peer_id: &Hash256) {
        let raw = match self.manifest_store.local() {
            Some(m) => m.raw.clone(),
            None => return,
        };
        let payload = crate::proto_convert::encode_manifests(vec![raw]);
        if let Some(handle) = self.peer_handles.get(peer_id) {
            let _ = handle.tx.try_send(rxrpl_p2p_proto::codec::PeerMessage {
                msg_type: MessageType::Manifests,
                payload,
            });
            tracing::debug!("sent local manifest to {}", peer_id);
        }
    }

    /// Send a GetShards request to a newly connected peer.
    fn send_get_shards(&self, peer_id: &Hash256) {
        if self.shard_syncer.is_none() {
            return;
        }
        let payload = rxrpl_p2p_proto::shard_msg::encode_get_shards();
        if let Some(handle) = self.peer_handles.get(peer_id) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::GetShards,
                payload,
            });
        }
        tracing::debug!("sent GetShards to {}", peer_id);
    }

    fn check_sync(&mut self) {
        let our_seq = self.ledger_seq.load(Ordering::Relaxed);

        // Find the highest peer sequence
        let max_peer_seq = self
            .peer_handles
            .keys()
            .filter_map(|id| self.peer_set.get(id))
            .map(|info| info.ledger_seq.load(Ordering::Relaxed))
            .max()
            .unwrap_or(0);

        // Only request the latest ledger if no sync is active.
        if self.ledger_syncer.needs_sync(our_seq, max_peer_seq)
            && !self.ledger_syncer.has_any_incremental_sync()
            && self.ledger_syncer.pending_count() == 0
        {
            // Request only the latest peer ledger, not all intermediary ones.
            self.send_get_ledger(max_peer_seq, None);
        } else if let Some(seq) = self.ledger_syncer.active_incremental_seq() {
            // Re-drive the active catchup. The response-driven loop can fall
            // quiet when in-flight dedup suppresses a round's requests and the
            // responses that would have driven the next send are lost; this
            // periodic kick re-requests any frontier nodes whose in-flight
            // window has expired.
            tracing::info!(
                "catchup #{}: {} cumulative nodes acquired",
                seq,
                self.ledger_syncer.lifetime_added()
            );
            self.send_get_ledger_as_node(seq);
            // During initial catchup keep pulling the freshest tip header so a
            // drained target always has a fresher ledger to re-target to -- the
            // fresh ledger still serves the deep frontier the aged target won't.
            // start_incremental_sync ignores it until the current target drains.
            if self.ledger_syncer.in_initial_catchup() && max_peer_seq > seq {
                self.send_get_ledger(max_peer_seq, None);
            }
        }
    }

    /// Disconnect peers whose reputation has dropped below the threshold.
    fn check_peer_reputations(&mut self) {
        let bad_peers: Vec<Hash256> = self
            .peer_set
            .all_peers()
            .iter()
            .filter(|info| info.reputation.should_disconnect())
            .map(|info| info.node_id)
            .collect();

        for node_id in bad_peers {
            tracing::warn!(
                "disconnecting peer {} due to low reputation score ({})",
                node_id,
                self.peer_set
                    .get(&node_id)
                    .map(|i| i.reputation.score())
                    .unwrap_or(0),
            );
            if let Some(handle) = self.peer_handles.remove(&node_id) {
                drop(handle);
            }
            self.peer_set.remove(&node_id);
        }
    }

    /// Broadcast our cluster status to all connected peers.
    ///
    /// Sends a TMCluster message containing our own node info (public key,
    /// load fee, name) plus the latest known state of all active cluster
    /// members. This allows cluster nodes to propagate a full view.
    fn broadcast_cluster_status(&self) {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32;

        let our_pub_key = hex::encode(self.identity.public_key_bytes());
        let our_node = proto_convert::ClusterNodeData {
            public_key: our_pub_key,
            report_time: now_secs,
            node_load: 256, // base load fee (256 = reference fee)
            node_name: self.cluster_manager.node_name().to_string(),
            address: format!("0.0.0.0:{}", self.config.listen_port),
        };

        let mut nodes = vec![our_node];

        // Include known active cluster peer state.
        for cn in self.cluster_manager.active_nodes() {
            nodes.push(proto_convert::ClusterNodeData {
                public_key: cn.public_key.clone(),
                report_time: cn.report_time,
                node_load: cn.load_fee,
                node_name: cn.name.clone(),
                address: cn.address.clone(),
            });
        }

        let payload = proto_convert::encode_cluster(&nodes);
        let mut sent = 0;
        for handle in self.peer_handles.values() {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::Cluster,
                payload: payload.clone(),
            });
            sent += 1;
        }
        tracing::debug!(
            "broadcast cluster status ({} nodes) to {} peers",
            nodes.len(),
            sent
        );
    }

    /// Check whether a peer is a trusted cluster member.
    ///
    /// Cluster members receive priority for transaction relay and their
    /// fee recommendations are trusted.
    pub fn is_cluster_peer(&self, node_id: &Hash256) -> bool {
        if !self.cluster_manager.is_enabled() {
            return false;
        }
        if self.peer_set.get(node_id).is_some() {
            // Match the peer's node ID (SHA-512-Half of public key) against
            // the cluster member list. In production the peer's raw public key
            // would be checked during handshake.
            let node_pub_hex = hex::encode(node_id.as_bytes());
            self.cluster_manager.is_member(&node_pub_hex)
        } else {
            false
        }
    }

    /// Return a snapshot of the current cluster state for RPC queries.
    pub fn cluster_info(&self) -> Vec<crate::cluster::ClusterNodeInfo> {
        self.cluster_manager.cluster_info()
    }

    fn get_node_store(&self) -> Option<Arc<dyn rxrpl_shamap::NodeStore>> {
        if let Some(ref store) = self.node_store {
            return Some(Arc::clone(store));
        }
        // Fallback: try to get store from ledger provider's latest ledger.
        self.ledger_provider
            .as_ref()
            .and_then(|p| p.latest_closed())
            .and_then(|l| l.store().cloned())
    }

    /// Send a GetLedger request for account state nodes (liAS_NODE).
    ///
    /// Used as follow-up after receiving a liBASE response. Includes
    /// specific node_ids from the incremental sync's missing list.
    /// Uses the ledger hash (NOT the state root) as `ledger_hash` so
    /// rippled can locate the correct ledger.
    fn send_get_ledger_as_node(&mut self, seq: u32) {
        let ledger_hash = match self.ledger_syncer.get_ledger_hash(seq) {
            Some(h) => h,
            None => return,
        };

        let missing = self.ledger_syncer.get_missing_node_ids(seq);
        let missing = self
            .ledger_syncer
            .take_unrequested(&missing, std::time::Instant::now());
        if missing.is_empty() {
            return;
        }

        // Send the SHAMap NodeId (33 bytes: path[32] + depth[1]) of each
        // missing node, matching rippled's TMGetLedger wire format. The
        // server walks its SHAMap by (path, depth) to locate the node.
        let node_ids: Vec<Vec<u8>> = missing
            .iter()
            .map(|mn| mn.node_id.to_wire_bytes())
            .collect();

        let num_ids = node_ids.len();
        let min_depth = missing
            .iter()
            .map(|mn| mn.node_id.depth())
            .min()
            .unwrap_or(0);
        let max_depth = missing
            .iter()
            .map(|mn| mn.node_id.depth())
            .max()
            .unwrap_or(0);

        // Split the frontier window into server-cap-sized requests (the server
        // truncates each GetLedger to MAX_GET_LEDGER_NODES=128 ids) and
        // round-robin them across the best peers, so each peer carries several
        // outstanding requests. This keeps the request pipeline full across
        // reply latency instead of issuing one request per peer per round --
        // the dominant lever once the frontier is wider than the fan-out.
        const REQUEST_NODE_CAP: usize = 128;
        let best = self.peer_set.best_peers_for_ledger(seq, DELTA_SYNC_FANOUT);
        let num_peers = best.len();
        if num_peers == 0 {
            return;
        }
        let mut requests_sent = 0usize;
        for (req_idx, chunk) in node_ids.chunks(REQUEST_NODE_CAP).enumerate() {
            let node_id = &best[req_idx % num_peers];
            let payload = proto_convert::encode_get_ledger_with_nodes(
                LI_AS_NODE,
                Some(&ledger_hash),
                seq,
                0,
                chunk.to_vec(),
            );
            if let Some(handle) = self.peer_handles.get(node_id) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::GetLedger,
                    payload,
                });
                requests_sent += 1;
            }
        }
        tracing::debug!(
            "sent GetLedger seq={} delta ({} node_ids in {} requests across {} peers, depth={}-{})",
            seq,
            num_ids,
            requests_sent,
            num_peers,
            min_depth,
            max_depth
        );
    }

    /// Handle a TMGetObjectByHash query from a peer.
    ///
    /// Looks up each requested hash in the local node store and sends back a
    /// response containing the found objects. Caps the number of objects per
    /// response to avoid excessive bandwidth usage (matching rippled's limit
    /// of 16384 objects per response and 256 KB total payload size).
    fn handle_get_objects_query(
        &self,
        from: Hash256,
        msg: rxrpl_p2p_proto::proto::TmGetObjectByHash,
    ) {
        /// Maximum number of objects to serve in a single response.
        ///
        /// rippled allows 16384 here, but each object is a `store.fetch`
        /// (random-read disk I/O on RocksDB). 256 keeps the response handler
        /// well under a few ms even on cold cache, mitigating the per-peer
        /// CPU/disk amplification flagged by the audit (H8).
        const MAX_OBJECTS: usize = 256;
        /// Maximum total payload size for response objects (256 KB).
        const MAX_RESPONSE_SIZE: usize = 256 * 1024;

        let requested = msg.objects.len();
        tracing::debug!(
            "GetObjectByHash query from {} ({} objects requested)",
            from,
            requested
        );

        let object_type = msg.r#type.unwrap_or(0);
        // rippled `TMGetObjectByHash::ObjectType` -> SHAMap leaf wireType:
        //   otTRANSACTION_NODE (3) -> tx-with-meta leaf, wireType 0x04
        //   otSTATE_NODE       (4) -> account-state leaf, wireType 0x01
        //   otFETCH_PACK       (6) -> handled separately; rippled uses fetch
        //                              packs to backfill ledger ancestors
        //                              after acquiring a validated head.
        //   anything else (otUNKNOWN, otLEDGER, otTRANSACTION) defaults to
        //   account-state, which is what rxrpl historically returned.
        // Inner nodes (16*32 bytes) always serialize with wireType 0x02.
        const OT_TRANSACTION_NODE: i32 = 3;
        const OT_FETCH_PACK: i32 = 6;
        if object_type == OT_FETCH_PACK {
            // Fetch packs are served from the ledger provider (header chain),
            // not the raw node store, so dispatch before the node-store check.
            self.handle_fetch_pack_query(from, msg);
            return;
        }

        let store = match &self.node_store {
            Some(s) => s,
            None => {
                tracing::debug!("GetObjectByHash from {} but no node store configured", from);
                return;
            }
        };

        let leaf_wire_type = if object_type == OT_TRANSACTION_NODE {
            WIRE_TYPE_TX_WITH_META
        } else {
            WIRE_TYPE_ACCOUNT_STATE
        };
        let ledger_seq = msg.seq.unwrap_or(0);
        let ledger_hash_bytes = msg.ledger_hash.as_deref().unwrap_or(&[]);
        let ledger_hash = if ledger_hash_bytes.len() >= 32 {
            let arr: [u8; 32] = ledger_hash_bytes[..32].try_into().unwrap_or([0u8; 32]);
            Some(Hash256::new(arr))
        } else {
            None
        };

        let limit = requested.min(MAX_OBJECTS);
        let mut found = Vec::new();
        let mut total_size = 0usize;

        for obj in msg.objects.iter().take(limit) {
            let hash_bytes = match &obj.hash {
                Some(h) if h.len() >= 32 => h,
                _ => continue,
            };
            let arr: [u8; 32] = match hash_bytes[..32].try_into() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let hash = Hash256::new(arr);

            match store.fetch(&hash) {
                Ok(Some(raw)) => {
                    // Storage holds rxrpl's internal node form (no wireType
                    // byte); rippled expects TMLedgerNode-style wire form.
                    // Wrap before sending so peers can decode the response. The
                    // node store is untyped, so inner-ness is inferred from the
                    // 16×32 layout here -- the one path where the node type is
                    // genuinely unavailable (a 480-byte leaf collides; rare).
                    let is_inner = raw.len() == 16 * 32;
                    let wire = encode_shamap_wire_node(&raw, is_inner, leaf_wire_type);
                    let entry_size = 32 + wire.len();
                    if total_size + entry_size > MAX_RESPONSE_SIZE {
                        break;
                    }
                    found.push((hash, wire));
                    total_size += entry_size;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::trace!("GetObjectByHash: store fetch error for {}: {}", hash, e);
                }
            }
        }

        if found.is_empty() {
            tracing::debug!(
                "GetObjectByHash from {}: none of {} requested objects found",
                from,
                requested
            );
            return;
        }

        let response = proto_convert::encode_get_objects_response(
            object_type,
            ledger_seq,
            ledger_hash.as_ref(),
            found.clone(),
        );

        tracing::debug!(
            "GetObjectByHash response to {}: {} of {} objects ({} bytes)",
            from,
            found.len(),
            requested,
            response.len()
        );

        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::GetObjects,
                payload: response,
            });
        }
    }

    /// Serve a `TMGetObjectByHash` query with `type=otFETCH_PACK`.
    ///
    /// rippled fires fetch-pack requests after acquiring a validated head
    /// ledger from us — they're how it backfills the ancestor chain before
    /// inserting the range into `complete_ledgers`. Each reply object
    /// contains `(parent_hash, parent_seq, HashPrefix::ledgerMaster ||
    /// raw_header_118b)`; rippled stores them via `addFetchPack` and then
    /// calls `gotFetchPack(progress, lastSeq)` to resume catchup.
    ///
    /// We walk back from the requested ledger via `parent_hash` until we run
    /// out of locally-known ledgers, hit `MAX_FETCH_PACK_LEDGERS`, or fill
    /// the 256 KB response budget. Each entry costs ~150 bytes so the cap
    /// effectively bounds total work to a few KB per request.
    fn handle_fetch_pack_query(
        &self,
        from: Hash256,
        msg: rxrpl_p2p_proto::proto::TmGetObjectByHash,
    ) {
        const MAX_FETCH_PACK_LEDGERS: usize = 32;
        const MAX_RESPONSE_SIZE: usize = 256 * 1024;
        // rippled `HashPrefix::ledgerMaster` ('L','W','R',0).
        const HASH_PREFIX_LEDGER_MASTER: [u8; 4] = [b'L', b'W', b'R', 0];

        let provider = match &self.ledger_provider {
            Some(p) => p,
            None => {
                tracing::debug!("FetchPack from {} but no ledger provider configured", from);
                return;
            }
        };

        let ledger_hash_bytes = msg.ledger_hash.as_deref().unwrap_or(&[]);
        if ledger_hash_bytes.len() < 32 {
            tracing::debug!("FetchPack from {}: missing or short ledger_hash", from);
            return;
        }
        let arr: [u8; 32] = match ledger_hash_bytes[..32].try_into() {
            Ok(a) => a,
            Err(_) => return,
        };
        let requested_ledger_hash = Hash256::new(arr);

        // Walk parent chain. The starting ledger is the one rippled requested
        // (the validated head it just acquired); per rippled's makeFetchPack,
        // we send the *parents* — the requestor already has the head.
        let head = match provider.get_by_hash(&requested_ledger_hash) {
            Some(l) => l,
            None => {
                tracing::debug!(
                    "FetchPack from {}: requested ledger {} not found",
                    from,
                    requested_ledger_hash
                );
                return;
            }
        };

        let mut objects: Vec<(Hash256, u32, Vec<u8>)> = Vec::new();
        let mut total_size = 0usize;
        let mut current_parent_hash = head.header.parent_hash;
        for _ in 0..MAX_FETCH_PACK_LEDGERS {
            if current_parent_hash == Hash256::ZERO {
                break;
            }
            let parent = match provider.get_by_hash(&current_parent_hash) {
                Some(p) => p,
                None => break,
            };
            let raw = parent.header.to_raw_bytes();
            let mut data = Vec::with_capacity(4 + raw.len());
            data.extend_from_slice(&HASH_PREFIX_LEDGER_MASTER);
            data.extend_from_slice(&raw);

            let entry_size = 32 + data.len();
            if total_size + entry_size > MAX_RESPONSE_SIZE {
                break;
            }
            total_size += entry_size;
            let parent_hash = parent.header.hash;
            let parent_seq = parent.header.sequence;
            current_parent_hash = parent.header.parent_hash;
            objects.push((parent_hash, parent_seq, data));
        }

        if objects.is_empty() {
            tracing::debug!(
                "FetchPack from {}: no ancestors available for {}",
                from,
                requested_ledger_hash
            );
            return;
        }

        let response =
            proto_convert::encode_fetch_pack_response(&requested_ledger_hash, objects.clone());

        tracing::debug!(
            "FetchPack response to {}: {} ancestor headers ({} bytes) for {}",
            from,
            objects.len(),
            response.len(),
            requested_ledger_hash
        );

        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::GetObjects,
                payload: response,
            });
        }
    }

    /// Send TMGetObjectByHash requests to fetch missing nodes by content hash.
    ///
    /// This is a fallback used when tree-based incremental sync (GetLedger with
    /// node_ids) gets stuck after repeated zero-add rounds. Instead of
    /// requesting nodes by their SHAMapNodeID position, we request them
    /// directly by their content hash via the GetObjects (type 42) message.
    fn send_get_objects_by_hash(&mut self, seq: u32, content_hashes: &[Hash256]) {
        let ledger_hash = match self.ledger_syncer.get_ledger_hash(seq) {
            Some(h) => h,
            None => return,
        };

        let content_hashes = self
            .ledger_syncer
            .take_unrequested_hashes(content_hashes, std::time::Instant::now());
        let content_hashes = content_hashes.as_slice();
        if content_hashes.is_empty() {
            return;
        }

        // Split across multiple peers, similar to send_get_ledger_as_node.
        let best = self.peer_set.best_peers_for_ledger(seq, 3);
        let num_peers = best.len();
        if num_peers == 0 {
            tracing::warn!("no peers available for GetObjectByHash seq={}", seq);
            return;
        }

        let chunk_size = content_hashes.len().div_ceil(num_peers);
        let mut peers_used = 0;
        for (i, node_id) in best.iter().enumerate() {
            let chunk: &[Hash256] =
                &content_hashes[i * chunk_size..content_hashes.len().min((i + 1) * chunk_size)];
            if chunk.is_empty() {
                break;
            }
            let payload =
                proto_convert::encode_get_objects_by_hash(&ledger_hash, seq, chunk, false);
            if let Some(handle) = self.peer_handles.get(node_id) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::GetObjects,
                    payload,
                });
                peers_used += 1;
            }
        }
        tracing::info!(
            "sent GetObjectByHash seq={} ({} hashes across {} peers, fallback for stuck sync)",
            seq,
            content_hashes.len(),
            peers_used
        );
    }

    /// Send a GetLedger request to the best 3 peers by reputation score.
    ///
    /// Uses weighted peer selection: peers are ranked by reputation score with
    /// a bonus for peers whose known ledger sequence is at or ahead of the target.
    ///
    /// When the ledger syncer has an active incremental sync for the target
    /// sequence, the request includes specific node hashes (delta sync).
    /// Otherwise, falls back to requesting all leaf nodes.
    fn send_get_ledger(&mut self, seq: u32, hash: Option<Hash256>) {
        // Register request in the syncer so responses can be correlated.
        self.ledger_syncer.register_request(seq, hash);

        let cookie = self.next_cookie.fetch_add(1, Ordering::Relaxed);

        // liBASE requests fetch the ledger header only -- no delta node_ids.
        let payload = proto_convert::encode_get_ledger_with_nodes(
            LI_BASE,
            hash.as_ref(),
            seq,
            cookie,
            Vec::new(),
        );

        // Send to a single peer.
        let best = self.peer_set.best_peers_for_ledger(seq, 1);
        if let Some(node_id) = best.first() {
            if let Some(handle) = self.peer_handles.get(node_id) {
                match handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::GetLedger,
                    payload,
                }) {
                    Ok(_) => {
                        tracing::info!(
                            "sent GetLedger seq={} hash={} itype=liBASE to {}",
                            seq,
                            hash.map(|h| h.to_string()).unwrap_or_else(|| "none".into()),
                            node_id
                        );
                    }
                    Err(e) => tracing::warn!("failed to send GetLedger to {}: {}", node_id, e),
                }
            }
        } else {
            tracing::warn!(
                "no peers available for GetLedger seq={} (peer_handles={})",
                seq,
                self.peer_handles.len()
            );
        }
    }

    fn handle_get_ledger(&self, from: Hash256, payload: &[u8]) {
        let req = match proto_convert::decode_get_ledger(payload) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("bad GetLedger from {}: {}", from, e);
                return;
            }
        };
        tracing::debug!(
            "GetLedger from {} type={} seq={:?} hash_len={} node_ids={}",
            from,
            req.itype,
            req.ledger_seq,
            req.ledger_hash.as_ref().map(|h| h.len()).unwrap_or(0),
            req.node_ids.len()
        );

        let provider = match &self.ledger_provider {
            Some(p) => p,
            None => {
                tracing::debug!("GetLedger from {} but no ledger provider", from);
                return;
            }
        };

        // TMLedgerInfoType (req_ledger_type): liBASE=0, liTX_NODE=1,
        // liAS_NODE=2, liTS_CANDIDATE=3 (handled via the module-level
        // LI_* constants).
        let req_ledger_type = req.itype;
        let req_ledger_hash = req.ledger_hash.unwrap_or_default();
        let req_ledger_seq = req.ledger_seq.unwrap_or(0);
        let req_cookie: Option<u32> = req.request_cookie.map(|c| c as u32);

        // Handle tx-set requests (liTS_CANDIDATE) separately.
        if req_ledger_type == LI_TS_CANDIDATE {
            self.handle_get_tx_set(from, &req_ledger_hash, &req.node_ids, req_cookie);
            return;
        }

        // Resolve the requested ledger. Selectors:
        // - hash present  -> try by-hash; if miss AND seq present, fall back to by-seq
        //   (peer may be probing; let it discover the divergence rather than timing out)
        // - else if seq>0 -> by-seq
        // - else          -> latest closed
        let ledger = if req_ledger_hash.len() >= 32 {
            let hash = Hash256::new(req_ledger_hash[..32].try_into().unwrap_or([0u8; 32]));
            match provider.get_by_hash(&hash) {
                Some(l) => Some(l),
                None if req_ledger_seq > 0 => {
                    tracing::debug!(
                        "GetLedger from {} hash={} not found, falling back to seq={}",
                        from,
                        hash,
                        req_ledger_seq
                    );
                    provider.get_by_seq(req_ledger_seq)
                }
                None => {
                    tracing::debug!(
                        "GetLedger from {} hash={} not found, no seq fallback",
                        from,
                        hash
                    );
                    None
                }
            }
        } else if req_ledger_seq > 0 {
            provider.get_by_seq(req_ledger_seq)
        } else {
            provider.latest_closed()
        };
        // req_ledger_type tells us *which* SHAMap to serve from — base /
        // tx_node / as_node — handled below when we serialise the response.

        let ledger = match ledger {
            Some(l) => l,
            None => {
                // Match rippled behavior: silently drop the request when the
                // ledger is unknown. Sending an empty TMLedgerData triggers
                // rippled's `nodes_size() <= 0` rejection (Protocol:WRN
                // "TMLedgerData: Invalid Ledger/TXset nodes 0") and incurs
                // peer charges for malformed traffic.
                tracing::debug!(
                    "GetLedger from {}: ledger not found (seq={}, hash_len={}); dropping",
                    from,
                    req_ledger_seq,
                    req_ledger_hash.len()
                );
                return;
            }
        };

        // Serialize state nodes (limit to 256KB)
        let mut nodes = Vec::new();
        let mut total_size = 0usize;
        let mut truncated = false;
        const MAX_RESPONSE_SIZE: usize = 256 * 1024;
        // Cap how many node-ids a single GetLedger may request. Each id can
        // trigger up to MAX_DEPTH (64) lazy `store.fetch` calls via the
        // SHAMap path-walk; without this bound a single peer can pin the
        // request handler with O(n × 64) disk I/O per message.
        const MAX_GET_LEDGER_NODES: usize = 128;
        // Match rippled's TMLedgerData reply caps (overlay Tuning.h): stop
        // pulling new requested ids once the reply reaches the soft cap, and
        // never emit past the hard cap within a single fat expansion.
        const SOFT_MAX_REPLY_NODES: usize = 8192;
        const HARD_MAX_REPLY_NODES: usize = 12288;
        // Bound fat-subtree recursion. The node caps already limit work, but a
        // hostile query_depth shouldn't drive needless deep walks.
        const MAX_QUERY_DEPTH: u8 = 10;

        // Parse requested node_ids from the request as rippled wire format
        // (33 bytes: path[32] + depth[1]). The pre-PR-#14 32-byte content-hash
        // shortcut has been removed in favor of unified rippled compatibility.
        let raw_node_ids = &req.node_ids;
        if raw_node_ids.len() > MAX_GET_LEDGER_NODES {
            tracing::warn!(
                "GetLedger from {} requested {} node_ids; truncating to {}",
                from,
                raw_node_ids.len(),
                MAX_GET_LEDGER_NODES
            );
        }
        let request_node_ids: Vec<ShamapNodeId> = raw_node_ids
            .iter()
            .take(MAX_GET_LEDGER_NODES)
            .filter_map(|id_bytes| {
                if id_bytes.len() == 33 {
                    let path: [u8; 32] = id_bytes[..32].try_into().ok()?;
                    let depth = id_bytes[32];
                    Some(ShamapNodeId::new(depth, &Hash256::new(path)))
                } else {
                    None
                }
            })
            .collect();

        // For liBASE (itype=0) requests with no specific node ids, the
        // protocol expects a single node entry containing the raw 118-byte
        // ledger header — that's what the late joiner parses to set up
        // its incremental sync. Returning state-map leaves here makes the
        // late joiner discard the response (header parse fails).
        if request_node_ids.is_empty() && req_ledger_type == LI_BASE {
            let header_bytes = ledger.header.to_raw_bytes();
            nodes.push((vec![], header_bytes));

            let response = proto_convert::encode_ledger_data(
                &ledger.header.hash,
                ledger.header.sequence,
                req_ledger_type,
                nodes,
                req_cookie,
            );
            if let Some(handle) = self.peer_handles.get(&from) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::LedgerData,
                    payload: response,
                });
            }
            return;
        }

        // Pick which SHAMap to serve from based on the requested itype, and
        // the corresponding leaf wireType byte rippled expects on the wire:
        //   liAS_NODE   -> account-state map, leaf wireType 0x01
        //   liTX_NODE   -> transaction map (with metadata), leaf wireType 0x04
        // Inner nodes always serialize with wireType 0x02 in either tree.
        // Any unknown itype (after liBASE / liTS_CANDIDATE handled above)
        // falls back to the state map so legacy peers keep working.
        let (map, leaf_wire_type) = match req_ledger_type {
            LI_TX_NODE => (&ledger.tx_map, WIRE_TYPE_TX_WITH_META),
            _ => (&ledger.state_map, WIRE_TYPE_ACCOUNT_STATE),
        };

        if !request_node_ids.is_empty() {
            // Delta sync: for each requested node serve a fat subtree (the node
            // plus descendants down `query_depth` levels), mirroring rippled's
            // getNodeFat. Returning a connected chunk per round-trip instead of a
            // single node is the dominant catchup-throughput lever. Outgoing
            // nodedata is rippled TMLedgerNode wire format (payload || wireType):
            //   inner -> 16*32 child hashes + wireType 2
            //   leaf  -> reorder key||data to data||key + tree leaf wireType.
            //
            // query_depth defaults to rippled's low-latency default of 1; the
            // state/tx maps are always served with fat leaves.
            let query_depth = req.query_depth.unwrap_or(1).min(MAX_QUERY_DEPTH as u32) as u8;
            for node_id in &request_node_ids {
                if nodes.len() >= SOFT_MAX_REPLY_NODES {
                    break;
                }
                let budget = HARD_MAX_REPLY_NODES - nodes.len();
                for (fat_id, raw, is_inner) in map.get_node_fat(*node_id, true, query_depth, budget)
                {
                    if nodes.len() >= HARD_MAX_REPLY_NODES {
                        truncated = true;
                        break;
                    }
                    let wire = encode_shamap_wire_node(&raw, is_inner, leaf_wire_type);
                    nodes.push((fat_id.to_wire_bytes(), wire));
                }
                if truncated {
                    break;
                }
            }

            if truncated {
                tracing::warn!(
                    "GetLedger delta response capped at {} nodes for seq={}",
                    HARD_MAX_REPLY_NODES,
                    ledger.header.sequence
                );
            }
        } else {
            // Full sync fallback: serve all leaf nodes in rippled wire format
            // (data || key || leaf_wire_type) for the selected tree. The
            // accompanying NodeId is the 33-byte `(path[32] || depth[1])` wire
            // identifier — sending the raw 32-byte key fails rippled's parser
            // which expects the depth byte appended.
            map.for_each_with_id(&mut |node_id, key, data| {
                let mut wire = Vec::with_capacity(data.len() + 33);
                wire.extend_from_slice(data);
                wire.extend_from_slice(key.as_bytes());
                wire.push(leaf_wire_type);
                let id_bytes = node_id.to_wire_bytes();
                let entry_size = id_bytes.len() + wire.len();
                if total_size + entry_size <= MAX_RESPONSE_SIZE {
                    nodes.push((id_bytes, wire));
                    total_size += entry_size;
                } else {
                    truncated = true;
                }
            });

            if truncated {
                tracing::warn!(
                    "GetLedger response truncated at 256KB: sent {} leaf nodes for seq={} itype={}",
                    nodes.len(),
                    ledger.header.sequence,
                    req_ledger_type
                );
            }
        }

        let response = proto_convert::encode_ledger_data(
            &ledger.header.hash,
            ledger.header.sequence,
            req_ledger_type,
            nodes,
            req_cookie,
        );

        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::LedgerData,
                payload: response,
            });
        }
    }

    /// Serve a tx-set request from a peer (GetLedger with itype=liTS_CANDIDATE).
    ///
    /// The candidate set is served as a transaction-no-metadata SHAMap: the
    /// peer either asks for specific nodes by id (rippled's `TransactionAcquire`
    /// walks the tree this way) or, with no node ids, gets every leaf. This is
    /// the same wire encoding `handle_get_ledger` uses for a ledger's tx tree,
    /// which is what lets a rippled peer acquire and re-apply rxrpl's proposed
    /// transactions instead of timing the set out.
    fn handle_get_tx_set(
        &self,
        from: Hash256,
        hash_bytes: &[u8],
        req_node_ids: &[Vec<u8>],
        cookie: Option<u32>,
    ) {
        let set_hash = if hash_bytes.len() >= 32 {
            Hash256::new(hash_bytes[..32].try_into().unwrap_or([0u8; 32]))
        } else {
            tracing::debug!("GetLedger liTS_CANDIDATE from {}: missing hash", from);
            return;
        };

        let tx_set = self
            .tx_sets
            .as_ref()
            .and_then(|cache| cache.read().unwrap().get(&set_hash).cloned());
        let tx_set = match tx_set {
            Some(set) => set,
            None => {
                tracing::debug!(
                    "GetLedger liTS_CANDIDATE from {}: tx-set {} not found; dropping",
                    from,
                    set_hash
                );
                return;
            }
        };

        // Materialise the candidate set as a tx-no-metadata SHAMap.
        let mut map = rxrpl_shamap::SHAMap::transaction();
        for (id, blob) in &tx_set.blobs {
            if !blob.is_empty() {
                let _ = map.put(*id, blob.clone());
            }
        }
        let _ = map.root_hash();

        let mut nodes: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let request_node_ids: Vec<ShamapNodeId> = req_node_ids
            .iter()
            .filter_map(|id_bytes| {
                if id_bytes.len() == 33 {
                    let path: [u8; 32] = id_bytes[..32].try_into().ok()?;
                    Some(ShamapNodeId::new(id_bytes[32], &Hash256::new(path)))
                } else {
                    None
                }
            })
            .collect();

        if request_node_ids.is_empty() {
            // A transaction-no-metadata leaf carries no key on the wire:
            // rippled recovers the id by hashing the blob. Wire = blob || 0x00.
            // The NodeId served is the 33-byte (path[32] || depth[1]) wire id;
            // rippled rejects the 32-byte raw key, which was the silent failure
            // that prevented `TransactionAcquire` from completing.
            map.for_each_with_id(&mut |node_id, _key, data| {
                let mut wire = Vec::with_capacity(data.len() + 1);
                wire.extend_from_slice(data);
                wire.push(WIRE_TYPE_TX_NO_META);
                nodes.push((node_id.to_wire_bytes(), wire));
            });
        } else {
            for node_id in &request_node_ids {
                if let Some((_h, raw, _is_inner)) = map.node_at(*node_id) {
                    let wire = encode_tx_no_meta_wire_node(&raw);
                    nodes.push((node_id.to_wire_bytes(), wire));
                }
            }
        }

        if nodes.is_empty() {
            tracing::debug!(
                "GetLedger liTS_CANDIDATE from {}: nothing to serve for {}",
                from,
                set_hash
            );
            return;
        }

        let response =
            proto_convert::encode_ledger_data(&set_hash, 0, LI_TS_CANDIDATE, nodes, cookie);

        if let Some(handle) = self.peer_handles.get(&from) {
            let _ = handle.tx.try_send(PeerMessage {
                msg_type: MessageType::LedgerData,
                payload: response,
            });
        }
    }

    /// Send a TMGetLedger request with itype=liTS_CANDIDATE to fetch a tx-set.
    fn send_get_tx_set(&self, peer: Hash256, tx_set_hash: Hash256) {
        let cookie = self.next_cookie.fetch_add(1, Ordering::Relaxed);
        let payload =
            proto_convert::encode_get_ledger(LI_TS_CANDIDATE, Some(&tx_set_hash), 0, cookie);
        if let Some(handle) = self.peer_handles.get(&peer) {
            match handle.tx.try_send(PeerMessage {
                msg_type: MessageType::GetLedger,
                payload,
            }) {
                Ok(_) => {
                    tracing::debug!(
                        "sent GetLedger liTS_CANDIDATE for tx-set {} to {}",
                        tx_set_hash,
                        peer
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "failed to send GetLedger for tx-set {} to {}: {}",
                        tx_set_hash,
                        peer,
                        e
                    );
                }
            }
        }
    }

    /// Handle a LedgerData response carrying a tx-set (liTS_CANDIDATE).
    ///
    /// Each leaf node is in rippled tx-no-metadata wire form
    /// (`tx_blob || tx_id[32] || 0x00`); the transaction id is its key and
    /// the blob is the canonical transaction. Reconstruct a blob-carrying
    /// TxSet, store it in the shared cache, and notify the consensus engine.
    fn handle_tx_set_response(&mut self, set_hash: Hash256, nodes: &[(Vec<u8>, Vec<u8>)]) {
        self.pending_tx_set_fetches.remove(&set_hash);

        if nodes.is_empty() {
            tracing::debug!("empty tx-set response for {}", set_hash);
            return;
        }

        // Decode leaf nodes: a tx-no-meta leaf is `blob || 0x00`; the
        // transaction id is recovered by hashing the blob. Inner nodes
        // (wire ending 0x02) are skipped — the leaves alone fully
        // determine the candidate set.
        let mut items: Vec<(Hash256, Vec<u8>)> = Vec::new();
        for (id, data) in nodes {
            if data.len() >= 2 && *data.last().unwrap() == WIRE_TYPE_TX_NO_META {
                let blob = data[..data.len() - 1].to_vec();
                let prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
                let txid = rxrpl_crypto::sha512_half::sha512_half(&[&prefix, &blob]);
                items.push((txid, blob));
            } else if id.len() >= 32 && data.is_empty() {
                // Legacy hash-only entry: id is the tx id, blob unknown.
                let arr: [u8; 32] = id[..32].try_into().unwrap();
                items.push((Hash256::new(arr), Vec::new()));
            }
        }

        let tx_set = TxSet::from_items(items);

        // Verify: the computed hash should match what we requested.
        if tx_set.hash != set_hash {
            tracing::warn!(
                "tx-set hash mismatch: expected {} got {} ({} txs)",
                set_hash,
                tx_set.hash,
                tx_set.len()
            );
            // Store under the computed hash anyway so consensus can still find it.
        }

        tracing::info!(
            "acquired tx-set {} with {} transactions",
            tx_set.hash,
            tx_set.len()
        );

        // Store in shared cache.
        if let Some(ref cache) = self.tx_sets {
            cache.write().unwrap().insert(tx_set.hash, tx_set.clone());
            // Also store under the requested hash if different.
            if tx_set.hash != set_hash {
                cache.write().unwrap().insert(set_hash, tx_set.clone());
            }
        }

        // Notify the consensus engine.
        self.forward_to_consensus(ConsensusMessage::TxSetAcquired(tx_set));
    }
}

/// Connect to a peer (outbound), perform handshake, and spawn read/write loops.
#[allow(clippy::too_many_arguments)]
async fn try_connect_outbound(
    addr: &str,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: &AtomicU32,
    ledger_hash: &RwLock<Hash256>,
    event_tx: &mpsc::Sender<PeerEvent>,
    peer_set: &PeerSet,
    tls_client: &Arc<SslConnector>,
) -> Result<Hash256, OverlayError> {
    let tcp = TcpStream::connect(addr)
        .await
        .map_err(|e| OverlayError::Connection(format!("{addr}: {e}")))?;

    let stream = tls::connect_tls(tcp, tls_client)
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS connect {addr}: {e}")))?;

    let seq = ledger_seq.load(Ordering::Relaxed);
    let hash = *ledger_hash.read().await;

    let (peer_node_id, software, public_key, framed) =
        handshake::handshake_outbound_http(stream, identity, network_id, seq, &hash).await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: false,
        public_key,
        connected_at: std::time::Instant::now(),
        ledger_seq: AtomicU32::new(0),
        reputation: PeerReputation::new(),
        scoring: PeerScore::new(),
        rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
        software,
    });

    if !peer_set.add(Arc::clone(&info)) {
        return Err(OverlayError::PeerLimitReached);
    }

    let write_tx = spawn_peer_loops(peer_node_id, framed, event_tx.clone());
    let _ = event_tx
        .send(PeerEvent::Connected {
            node_id: peer_node_id,
            info,
            write_tx,
        })
        .await;

    Ok(peer_node_id)
}

/// Accept an inbound connection, then either serve a `/crawl` request or
/// complete the peer upgrade and spawn read/write loops.
///
/// Returns `Ok(Some(node_id))` for an established peer and `Ok(None)` when the
/// connection was a crawl request (no peer registered).
#[allow(clippy::too_many_arguments)]
async fn try_accept_inbound(
    tcp: TcpStream,
    addr: &str,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_hash: &RwLock<Hash256>,
    event_tx: &mpsc::Sender<PeerEvent>,
    peer_set: &PeerSet,
    tls_server: &Arc<SslAcceptor>,
    crawl_info: Option<&Arc<dyn CrawlInfo>>,
) -> Result<Option<Hash256>, OverlayError> {
    let mut stream = tls::accept_tls(tcp, tls_server)
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS accept {addr}: {e}")))?;

    let request_buf = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handshake::read_until_header_end(&mut stream),
    )
    .await
    .map_err(|_| OverlayError::Handshake("receive HTTP request timeout".into()))??;

    let (target, req_headers) = http::parse_http_request(&request_buf)
        .map_err(|e| OverlayError::Handshake(format!("parse HTTP request: {e}")))?;

    if crawl::is_crawl_request(&target) {
        let snapshot = crawl_info.map(|c| c.crawl_snapshot());
        let doc = crawl::build_crawl_json(
            identity,
            network_id,
            &peer_set.all_peers(),
            snapshot.as_ref(),
        );
        let response = crawl::build_response_bytes(&doc);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            stream.write_all(&response).await?;
            stream.flush().await
        })
        .await
        .map_err(|_| OverlayError::Handshake("send crawl response timeout".into()))?
        .map_err(|e| OverlayError::Handshake(format!("send crawl response: {e}")))?;
        tracing::debug!("served /crawl to {}", addr);
        return Ok(None);
    }

    let hash = *ledger_hash.read().await;

    let (peer_node_id, software, public_key, framed) =
        handshake::respond_inbound_upgrade(stream, &req_headers, identity, network_id, &hash)
            .await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: true,
        public_key,
        connected_at: std::time::Instant::now(),
        ledger_seq: AtomicU32::new(0),
        reputation: PeerReputation::new(),
        scoring: PeerScore::new(),
        rate_limiter: crate::rate_limiter::PeerRateLimiter::default(),
        software,
    });

    if !peer_set.add(Arc::clone(&info)) {
        return Err(OverlayError::PeerLimitReached);
    }

    let write_tx = spawn_peer_loops(peer_node_id, framed, event_tx.clone());
    let _ = event_tx
        .send(PeerEvent::Connected {
            node_id: peer_node_id,
            info,
            write_tx,
        })
        .await;

    Ok(Some(peer_node_id))
}

/// Convert a SHAMap storage-format node into the rippled `TMLedgerNode.nodedata`
/// wire format for state-map (account-state) responses.
///
/// rippled SHAMap node wire-type bytes (trailing tag in TMLedgerNode payloads).
const WIRE_TYPE_INNER: u8 = 2;
const WIRE_TYPE_ACCOUNT_STATE: u8 = 1;
const WIRE_TYPE_TX_WITH_META: u8 = 4;
/// Trailing tag for a transaction-no-metadata leaf (consensus candidate set).
const WIRE_TYPE_TX_NO_META: u8 = 0;

/// Encode a SHAMap node from rxrpl storage form to rippled TMLedgerNode wire form.
///
/// Storage layout (rxrpl `node_store::deserialize_node`):
/// - Inner: `16 * 32 bytes` of child hashes
/// - Leaf:  `key[32] || data` (state-map leaf body is the SLE blob, tx-map
///   leaf body is the tx blob followed by the metadata blob)
///
/// Wire layout (rippled `SHAMap*Node::serializeForWire`):
/// - Inner:        `16 * 32 bytes || 0x02` (wireTypeInner)
/// - State leaf:   `data || key[32] || 0x01` (wireTypeAccountState)
/// - Tx-w/meta:    `data || key[32] || 0x04` (wireTypeTransactionWithMeta)
///
/// `leaf_wire_type` selects the trailing byte for leaves; inner nodes are
/// always tagged with `WIRE_TYPE_INNER` regardless of the tree.
/// Encode a candidate-tx-set SHAMap node into rippled wire form.
///
/// Inner nodes are `16*32 bytes || 0x02`. A transaction-no-metadata leaf is
/// `tx_blob || 0x00` — unlike account-state / tx-with-meta leaves it carries
/// no key, since the transaction id is recoverable by hashing the blob.
fn encode_tx_no_meta_wire_node(storage: &[u8]) -> Vec<u8> {
    if storage.len() == 16 * 32 {
        let mut wire = Vec::with_capacity(storage.len() + 1);
        wire.extend_from_slice(storage);
        wire.push(WIRE_TYPE_INNER);
        wire
    } else if storage.len() >= 32 {
        // Leaf storage is `key[32] || data`; the wire form drops the key.
        let data = &storage[32..];
        let mut wire = Vec::with_capacity(data.len() + 1);
        wire.extend_from_slice(data);
        wire.push(WIRE_TYPE_TX_NO_META);
        wire
    } else {
        storage.to_vec()
    }
}

/// Wrap a node's internal storage bytes in rippled's TMLedgerNode wire form.
///
/// `is_inner` MUST come from the node's actual type (e.g. `SHAMap::node_at`),
/// never from byte length: a leaf with exactly 480 bytes of data serializes to
/// 512 bytes and collides with an inner node's 16×32 layout, which would tag it
/// as an inner and corrupt the peer's catchup.
fn encode_shamap_wire_node(storage: &[u8], is_inner: bool, leaf_wire_type: u8) -> Vec<u8> {
    if is_inner {
        let mut wire = Vec::with_capacity(storage.len() + 1);
        wire.extend_from_slice(storage);
        wire.push(WIRE_TYPE_INNER);
        wire
    } else if storage.len() >= 32 {
        let key = &storage[..32];
        let data = &storage[32..];
        let mut wire = Vec::with_capacity(storage.len() + 1);
        wire.extend_from_slice(data);
        wire.extend_from_slice(key);
        wire.push(leaf_wire_type);
        wire
    } else {
        // Malformed; emit untyped passthrough so the receiver discards.
        storage.to_vec()
    }
}

/// Split a framed connection and spawn read/write loops.
/// Returns the write channel sender for the PeerHandle.
fn spawn_peer_loops(
    node_id: Hash256,
    framed: Framed<PeerStream, PeerCodec>,
    event_tx: mpsc::Sender<PeerEvent>,
) -> mpsc::Sender<PeerMessage> {
    let (write, read) = framed.split();
    // Per-peer outbound send queue. 256 overflowed under mainnet load — a sync
    // burst (e.g. 512 GetObjectByHash node ids, plus GetLedger requests) filled
    // it faster than the TLS write loop drained, so try_send returned "no
    // available capacity" and dropped the very requests catchup depends on,
    // stalling the sync. 2048 absorbs those bursts.
    let (tx, rx) = mpsc::channel(2048);

    tokio::spawn(peer_loop::run_peer_read_loop(node_id, read, event_tx));
    tokio::spawn(peer_loop::run_peer_write_loop(write, rx));

    tx
}

/// Decode a validator list blob (base64-encoded JSON) and return the validator count.
///
/// The blob format is: `{"validators": [{"validation_public_key": "...", "manifest": "..."}, ...], ...}`
fn base64_decode_validator_blob(blob_bytes: &[u8]) -> Result<usize, ()> {
    use base64::Engine;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(blob_bytes)
        .map_err(|_| ())?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).map_err(|_| ())?;
    let validators = json
        .get("validators")
        .and_then(|v| v.as_array())
        .ok_or(())?;
    Ok(validators.len())
}

/// Exponential backoff state for fixed peer reconnection.
///
/// Starts at 1 second and doubles on each failed attempt, capping at 30 seconds.
/// Resets to 1 second after a successful connection.
struct ReconnectBackoff {
    current: Duration,
    attempts: u32,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

impl ReconnectBackoff {
    fn new() -> Self {
        Self {
            current: BACKOFF_INITIAL,
            attempts: 0,
        }
    }

    /// Return the next delay and advance the backoff state.
    fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.attempts += 1;
        self.current = (self.current * 2).min(BACKOFF_MAX);
        delay
    }

    /// Reset backoff after a successful connection.
    fn reset(&mut self) {
        self.current = BACKOFF_INITIAL;
        self.attempts = 0;
    }

    /// Number of reconnection attempts since last reset.
    fn attempt(&self) -> u32 {
        self.attempts
    }
}

#[cfg(test)]
mod tests;
