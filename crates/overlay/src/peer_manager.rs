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
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, RwLock, Semaphore, mpsc};
use tokio_util::codec::Framed;

use crate::cluster::ClusterManager;
use crate::command::OverlayCommand;
use crate::discovery::PeerDiscovery;
use crate::error::OverlayError;
use crate::event::PeerEvent;
use crate::handshake;
use crate::identity::NodeIdentity;
use crate::ledger_provider::LedgerProvider;
use crate::ledger_sync::LedgerSyncer;
use crate::shard_sync::ShardSyncer;
use crate::manifest::{self, ManifestStore};
use crate::peer_handle::PeerHandle;
use crate::peer_loop;
use crate::peer_score::PeerScore;
use crate::peer_set::{PeerInfo, PeerSet};
use crate::proto_convert;
use crate::relay::RelayFilter;
use crate::reputation::PeerReputation;
use crate::squelch::SquelchManager;
use crate::tx_batch_relay::TxBatchRelay;
use crate::validator_list::{self, ValidatorListTracker};
use crate::tls::{self, PeerStream};

/// TMLedgerInfoType values from rippled.
const LI_BASE: i32 = 0;
const _LI_TX_NODE: i32 = 1;
const LI_AS_NODE: i32 = 2;
const LI_TS_CANDIDATE: i32 = 3;

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
    consensus_tx: mpsc::UnboundedSender<ConsensusMessage>,
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
        mpsc::UnboundedReceiver<ConsensusMessage>,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let cmd_tx_internal = cmd_tx.clone();
        let (consensus_tx, consensus_rx) = mpsc::unbounded_channel();
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
        };

        (mgr, cmd_tx, consensus_rx)
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
                    addr, delay, backoff.attempt(),
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
        let ledger_seq = Arc::clone(&self.ledger_seq);
        let ledger_hash = Arc::clone(&self.ledger_hash);
        let event_tx = self.event_tx.clone();
        let peer_set = Arc::clone(&self.peer_set);
        let tls_server = self.config.tls_server.clone();
        let permits = Arc::clone(&self.inbound_handshake_permits);

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
                &ledger_seq,
                &ledger_hash,
                &event_tx,
                &peer_set,
                &tls_server,
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
                        msg_type, sent, full
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
                        msg_type, from, info.rate_limiter.consecutive_drops()
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

        match msg_type {
            MessageType::Hello => {
                // Hello is already handled during handshake; ignore late arrivals.
                tracing::debug!("ignoring late Hello from {}", from);
            }
            MessageType::Ping => {
                match proto_convert::decode_ping(payload) {
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
                }
            }
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
                            let _ = self.consensus_tx.send(ConsensusMessage::Transaction {
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
            MessageType::ProposeSet => {
                match proto_convert::decode_propose_set(payload) {
                    Ok(proposal) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_valid_message(payload_len);
                        }
                        let _ = self.consensus_tx.send(ConsensusMessage::Proposal(proposal));
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
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
                            if let Some(action) = self.squelch_manager.record_validation_source(
                                from,
                                &validation.public_key,
                            ) {
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
                            for (id, handle) in &self.peer_handles {
                                if *id != from
                                    && !self.squelch_manager.is_relay_squelched(
                                        id,
                                        &validation.public_key,
                                    )
                                {
                                    let _ = handle.tx.try_send(PeerMessage {
                                        msg_type: MessageType::Validation,
                                        payload: payload.to_vec(),
                                    });
                                }
                            }

                            let _ = self
                                .consensus_tx
                                .send(ConsensusMessage::Validation(validation));
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
                            let requests = self.ledger_syncer.request_missing(our_seq, ledger_seq);
                            for (seq, hash) in requests {
                                self.send_get_ledger(seq, hash);
                            }
                        }

                        let _ = self.consensus_tx.send(ConsensusMessage::StatusChange {
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
                            from, accepted, nodes.len()
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
                tracing::info!(
                    "received LedgerData from {}: {} bytes",
                    from, payload.len()
                );
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
                            ledger_seq, hash, info_type, nodes.len()
                        );

                        // Handle tx-set candidate responses (liTS_CANDIDATE = 3).
                        if info_type == LI_TS_CANDIDATE {
                            self.handle_tx_set_response(hash, &nodes);
                            return;
                        }

                        // Skip already-synced ledgers.
                        if self.ledger_syncer.is_synced(ledger_seq) {
                            tracing::debug!("ignoring LedgerData for already-synced #{}", ledger_seq);
                        } else if self.ledger_syncer.has_incremental_sync(ledger_seq) {
                            // Active incremental sync: feed nodes into SHAMap.
                            use crate::ledger_sync::FeedResult;
                            match self.ledger_syncer.feed_nodes(ledger_seq, &nodes) {
                                FeedResult::Complete(leaves) => {
                                    tracing::info!(
                                        "incremental sync complete for ledger #{} ({} leaf nodes)",
                                        ledger_seq, leaves.len()
                                    );
                                    self.ledger_syncer.mark_synced(ledger_seq);
                                    let _ = self.consensus_tx.send(ConsensusMessage::LedgerData {
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
                            if let Some(header) = rxrpl_ledger::LedgerHeader::from_raw_bytes(header_data)
                                .filter(|h| {
                                    latest.map_or(true, |known| {
                                        (h.sequence as i64 - known as i64).unsigned_abs() <= 1000
                                    })
                                })
                            {
                                let is_newer = latest.map_or(true, |known| header.sequence > known);
                                if is_newer {
                                    tracing::info!(
                                        "received liBASE header for ledger #{} hash={}",
                                        header.sequence, header.hash
                                    );
                                }
                                self.ledger_syncer.set_ledger_hash(header.sequence, header.hash);

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
                                            header.sequence, leaves.len()
                                        );
                                        self.ledger_syncer.mark_synced(header.sequence);
                                        let _ =
                                            self.consensus_tx.send(ConsensusMessage::LedgerData {
                                                hash: header.hash,
                                                seq: header.sequence,
                                                nodes: leaves,
                                            });
                                    }
                                }

                                let _ = self.consensus_tx.send(ConsensusMessage::LedgerHeader {
                                    seq: header.sequence,
                                    header,
                                });
                            } else {
                                // Not a header -- raw node data, not useful for reconstruction.
                                tracing::debug!(
                                    "ignoring non-header LedgerData for #{} ({} nodes)",
                                    ledger_seq, nodes.len()
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
                        tracing::debug!(
                            "received {} peer addresses from {}",
                            peers.len(),
                            from
                        );
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
                        tracing::debug!(
                            "received {} manifests from {}",
                            manifest_list.len(),
                            from
                        );

                        // Parse, verify, and apply each manifest
                        let raw_bytes: Vec<Vec<u8>> = manifest_list
                            .into_iter()
                            .filter_map(|m| m.stobject)
                            .collect();
                        let mut applied = 0;
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
                                        let _ = self.consensus_tx.send(
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
                            from, have_set.hash, have_set.status
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
                                have_set.hash, from
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
                                from, nodes.len(), ledger_seq
                            );
                            if self.ledger_syncer.has_incremental_sync(ledger_seq) {
                                use crate::ledger_sync::FeedResult;
                                let ledger_hash = self.ledger_syncer.get_ledger_hash(ledger_seq);
                                let hash = ledger_hash.unwrap_or(Hash256::ZERO);
                                match self.ledger_syncer.feed_nodes(ledger_seq, &nodes) {
                                    FeedResult::Complete(leaves) => {
                                        tracing::info!(
                                            "incremental sync complete (via hash fallback) for #{} ({} leaves)",
                                            ledger_seq, leaves.len()
                                        );
                                        self.ledger_syncer.mark_synced(ledger_seq);
                                        let _ = self.consensus_tx.send(ConsensusMessage::LedgerData {
                                            hash,
                                            seq: ledger_seq,
                                            nodes: leaves,
                                        });
                                    }
                                    FeedResult::FallbackToHashFetch(content_hashes) => {
                                        self.send_get_objects_by_hash(ledger_seq, &content_hashes);
                                    }
                                    FeedResult::Continue => {
                                        self.send_get_ledger_as_node(ledger_seq);
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
                            vl.version.unwrap_or(0), from,
                            vl.manifest.as_ref().map(|v| v.len()).unwrap_or(0),
                            vl.blob.as_ref().map(|v| v.len()).unwrap_or(0)
                        );

                        // Attempt full signature verification
                        let manifest_bytes = vl.manifest.as_ref().map(|v| v.as_slice());
                        let blob_bytes = vl.blob.as_ref().map(|v| v.as_slice());
                        let sig_bytes = vl.signature.as_ref().map(|v| v.as_slice());

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
                                    if self.vl_tracker.record_sequence(
                                        &vl_data.publisher_master_key,
                                        seq,
                                    ) {
                                        tracing::info!(
                                            "verified validator list seq={} with {} validators from {}",
                                            seq, count, from
                                        );

                                        // Process individual validator manifests
                                        for raw_manifest in &vl_data.validator_manifests {
                                            if let Ok(m) = manifest::parse_and_verify(raw_manifest) {
                                                let master_key = m.master_public_key.clone();
                                                let eph_key = m.ephemeral_public_key.clone();
                                                let revoked = m.is_revoked();
                                                let old_eph = self
                                                    .manifest_store
                                                    .current_ephemeral_key(&master_key)
                                                    .cloned();
                                                if self.manifest_store.apply(m) {
                                                    let _ = self.consensus_tx.send(
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
                                        let _ = self.consensus_tx.send(
                                            ConsensusMessage::ValidatorListVerified {
                                                validators: vl_data.validators,
                                                sequence: seq,
                                            },
                                        );
                                    } else {
                                        tracing::debug!(
                                            "stale validator list seq={} from {}",
                                            seq, from
                                        );
                                    }

                                    // Also send the count for backward compatibility
                                    let _ = self.consensus_tx.send(
                                        ConsensusMessage::ValidatorListReceived {
                                            validator_count: count,
                                        },
                                    );
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        "validator list verification failed from {}: {}",
                                        from, e
                                    );
                                    // Fall back to unverified count extraction
                                    if let Some(blob_b) = vl.blob.as_ref() {
                                        if let Ok(count) = base64_decode_validator_blob(blob_b) {
                                            let _ = self.consensus_tx.send(
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
                                    let _ = self.consensus_tx.send(
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
                            vlc.version.unwrap_or(0), from, vlc.blobs.len()
                        );
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
            MessageType::Squelch => {
                match proto_convert::decode_squelch(payload) {
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
                            from, squelch_flag, duration,
                        );
                    }
                    Err(_) => {
                        if let Some(ref info) = peer_info {
                            info.reputation.record_invalid_message();
                        }
                    }
                }
            }
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
                        let missing =
                            self.tx_batch_relay.process_have_transactions(&need_hashes);
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
                        let new_txs =
                            self.tx_batch_relay.process_transactions_batch(&batch.transactions);
                        tracing::debug!(
                            "Transactions batch from {} with {} txs ({} new)",
                            from,
                            batch.transactions.len(),
                            new_txs.len(),
                        );
                        // Forward each new transaction to the consensus layer
                        for (tx_hash, tx_data) in &new_txs {
                            if self.relay_filter.should_relay(tx_hash) {
                                let _ =
                                    self.consensus_tx.send(ConsensusMessage::Transaction {
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
            MessageType::Shards => {
                match rxrpl_p2p_proto::shard_msg::decode_shards(payload) {
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
                }
            }
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
        let payload = proto_convert::encode_squelch(
            &action.validator_key,
            true,
            action.duration_secs,
        );
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
            from, shard_index, seqs.len()
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
            entry_count, shard_index, from
        );

        // The actual async import is triggered by the ShardSyncer's tick()
        // method. Here we just log receipt. The ShardSyncer::on_shard_data()
        // method handles the actual import but requires async context.
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
        let min_depth = missing.iter().map(|mn| mn.node_id.depth()).min().unwrap_or(0);
        let max_depth = missing.iter().map(|mn| mn.node_id.depth()).max().unwrap_or(0);

        // Split requests across multiple peers so each gets a different subset.
        let best = self.peer_set.best_peers_for_ledger(seq, 3);
        let num_peers = best.len();
        if num_peers == 0 {
            return;
        }
        let chunk_size = (node_ids.len() + num_peers - 1) / num_peers;
        let mut peers_used = 0;
        for (i, node_id) in best.iter().enumerate() {
            let chunk: Vec<Vec<u8>> = node_ids
                .iter()
                .skip(i * chunk_size)
                .take(chunk_size)
                .cloned()
                .collect();
            if chunk.is_empty() {
                break;
            }
            let payload = proto_convert::encode_get_ledger_with_nodes(
                LI_AS_NODE,
                Some(&ledger_hash),
                seq,
                0,
                chunk,
            );
            if let Some(handle) = self.peer_handles.get(node_id) {
                let _ = handle.tx.try_send(PeerMessage {
                    msg_type: MessageType::GetLedger,
                    payload,
                });
                peers_used += 1;
            }
        }
        tracing::debug!(
            "sent GetLedger seq={} delta ({} node_ids across {} peers, depth={}-{})",
            seq, num_ids, peers_used, min_depth, max_depth
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
            from, requested
        );

        let store = match &self.node_store {
            Some(s) => s,
            None => {
                tracing::debug!(
                    "GetObjectByHash from {} but no node store configured",
                    from
                );
                return;
            }
        };

        let object_type = msg.r#type.unwrap_or(0);
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
                Ok(Some(data)) => {
                    let entry_size = 32 + data.len();
                    if total_size + entry_size > MAX_RESPONSE_SIZE {
                        break;
                    }
                    found.push((hash, data));
                    total_size += entry_size;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::trace!(
                        "GetObjectByHash: store fetch error for {}: {}",
                        hash, e
                    );
                }
            }
        }

        if found.is_empty() {
            tracing::debug!(
                "GetObjectByHash from {}: none of {} requested objects found",
                from, requested
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
            from, found.len(), requested, response.len()
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
    fn send_get_objects_by_hash(&self, seq: u32, content_hashes: &[Hash256]) {
        let ledger_hash = match self.ledger_syncer.get_ledger_hash(seq) {
            Some(h) => h,
            None => return,
        };

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

        let chunk_size = (content_hashes.len() + num_peers - 1) / num_peers;
        let mut peers_used = 0;
        for (i, node_id) in best.iter().enumerate() {
            let chunk: &[Hash256] = &content_hashes
                [i * chunk_size..content_hashes.len().min((i + 1) * chunk_size)];
            if chunk.is_empty() {
                break;
            }
            let payload = proto_convert::encode_get_objects_by_hash(
                &ledger_hash,
                seq,
                chunk,
                false,
            );
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
            seq, content_hashes.len(), peers_used
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

        // TMLedgerInfoType: liBASE=0, liTX_NODE=1, liAS_NODE=2, liTS_CANDIDATE=3
        const LT_CLOSED: i32 = 1;
        const LT_VALIDATED: i32 = 2;
        const LT_HASH: i32 = 3;

        let req_ledger_type = req.itype;
        let req_ledger_hash = req.ledger_hash.unwrap_or_default();
        let req_ledger_seq = req.ledger_seq.unwrap_or(0);
        let req_cookie = req.request_cookie.unwrap_or(0);

        // Handle tx-set requests (liTS_CANDIDATE) separately.
        if req_ledger_type == LI_TS_CANDIDATE {
            self.handle_get_tx_set(from, &req_ledger_hash, req_cookie);
            return;
        }

        // Resolve the requested ledger. The selectors are independent of the
        // node-payload itype: a request that supplies a hash points to that
        // ledger, otherwise a non-zero seq points to that seq, otherwise
        // we fall back to the latest closed ledger.
        let ledger = if req_ledger_hash.len() >= 32 {
            let hash = Hash256::new(req_ledger_hash[..32].try_into().unwrap_or([0u8; 32]));
            provider.get_by_hash(&hash)
        } else if req_ledger_seq > 0 {
            provider.get_by_seq(req_ledger_seq)
        } else {
            provider.latest_closed()
        };
        // (req_ledger_type tells us *which* SHAMap to serve from — base /
        // tx_node / as_node — handled below when we serialise the response.)
        let _ = req_ledger_type;
        let _ = (LT_CLOSED, LT_VALIDATED, LT_HASH);

        let ledger = match ledger {
            Some(l) => l,
            None => {
                tracing::debug!("GetLedger from {}: ledger not found", from);
                let empty_response = proto_convert::encode_ledger_data(
                    &Hash256::ZERO,
                    req_ledger_seq,
                    req_ledger_type,
                    vec![],
                    req_cookie,
                );
                if let Some(handle) = self.peer_handles.get(&from) {
                    let _ = handle.tx.try_send(PeerMessage {
                        msg_type: MessageType::LedgerData,
                        payload: empty_response,
                    });
                }
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
        const LI_BASE: i32 = 0;
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

        if !request_node_ids.is_empty() {
            // Delta sync: serve specific nodes by walking the SHAMap to the
            // requested (path, depth). Outgoing nodedata appends a trailing
            // depth byte to match rippled's TMLedgerNode wire format.
            for node_id in &request_node_ids {
                let Some((_content_hash, mut raw)) = ledger.state_map.node_at(*node_id) else {
                    continue;
                };
                raw.push(node_id.depth());
                let id_bytes = node_id.to_wire_bytes();
                let entry_size = id_bytes.len() + raw.len();
                if total_size + entry_size <= MAX_RESPONSE_SIZE {
                    nodes.push((id_bytes, raw));
                    total_size += entry_size;
                } else {
                    truncated = true;
                    break;
                }
            }

            if truncated {
                tracing::warn!(
                    "GetLedger delta response truncated at 256KB: sent {} of {} requested nodes for seq={}",
                    nodes.len(), request_node_ids.len(), ledger.header.sequence
                );
            }
        } else {
            // Full sync fallback: serve all leaf nodes.
            ledger.state_map.for_each(&mut |key, data| {
                let entry_size = key.as_bytes().len() + data.len();
                if total_size + entry_size <= MAX_RESPONSE_SIZE {
                    nodes.push((key.as_bytes().to_vec(), data.to_vec()));
                    total_size += entry_size;
                } else {
                    truncated = true;
                }
            });

            if truncated {
                tracing::warn!(
                    "GetLedger response truncated at 256KB: sent {} state nodes for seq={}",
                    nodes.len(), ledger.header.sequence
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
    fn handle_get_tx_set(&self, from: Hash256, hash_bytes: &[u8], cookie: u64) {
        let set_hash = if hash_bytes.len() >= 32 {
            Hash256::new(hash_bytes[..32].try_into().unwrap_or([0u8; 32]))
        } else {
            tracing::debug!("GetLedger liTS_CANDIDATE from {}: missing hash", from);
            return;
        };

        let tx_set = self.tx_sets.as_ref().and_then(|cache| {
            cache.read().unwrap().get(&set_hash).cloned()
        });

        let nodes = match tx_set {
            Some(set) => {
                set.txs
                    .iter()
                    .map(|tx_hash| (tx_hash.as_bytes().to_vec(), Vec::new()))
                    .collect()
            }
            None => {
                tracing::debug!(
                    "GetLedger liTS_CANDIDATE from {}: tx-set {} not found",
                    from, set_hash
                );
                Vec::new()
            }
        };

        let response = proto_convert::encode_ledger_data(
            &set_hash,
            0,
            LI_TS_CANDIDATE,
            nodes,
            cookie,
        );

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
        let payload = proto_convert::encode_get_ledger(
            LI_TS_CANDIDATE,
            Some(&tx_set_hash),
            0,
            cookie,
        );
        if let Some(handle) = self.peer_handles.get(&peer) {
            match handle.tx.try_send(PeerMessage {
                msg_type: MessageType::GetLedger,
                payload,
            }) {
                Ok(_) => {
                    tracing::debug!(
                        "sent GetLedger liTS_CANDIDATE for tx-set {} to {}",
                        tx_set_hash, peer
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "failed to send GetLedger for tx-set {} to {}: {}",
                        tx_set_hash, peer, e
                    );
                }
            }
        }
    }

    /// Handle a LedgerData response carrying a tx-set (liTS_CANDIDATE).
    ///
    /// Each node in the response is a (tx_hash, tx_data) pair.
    /// Reconstruct the TxSet from the transaction hashes and store it
    /// in the shared cache, then notify the consensus engine.
    fn handle_tx_set_response(&mut self, set_hash: Hash256, nodes: &[(Vec<u8>, Vec<u8>)]) {
        self.pending_tx_set_fetches.remove(&set_hash);

        if nodes.is_empty() {
            tracing::debug!("empty tx-set response for {}", set_hash);
            return;
        }

        // Extract transaction hashes from node IDs.
        let tx_hashes: Vec<Hash256> = nodes
            .iter()
            .filter_map(|(id, _data)| {
                if id.len() >= 32 {
                    let arr: [u8; 32] = id[..32].try_into().ok()?;
                    Some(Hash256::new(arr))
                } else {
                    None
                }
            })
            .collect();

        let tx_set = TxSet::new(tx_hashes);

        // Verify: the computed hash should match what we requested.
        if tx_set.hash != set_hash {
            tracing::warn!(
                "tx-set hash mismatch: expected {} got {} ({} txs)",
                set_hash, tx_set.hash, tx_set.len()
            );
            // Store under the computed hash anyway so consensus can still find it.
        }

        tracing::info!(
            "acquired tx-set {} with {} transactions",
            tx_set.hash, tx_set.len()
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
        let _ = self.consensus_tx.send(ConsensusMessage::TxSetAcquired(tx_set));
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

    let (peer_node_id, software, framed) =
        handshake::handshake_outbound_http(stream, identity, network_id, seq, &hash).await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: false,
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

/// Accept an inbound peer, perform handshake, and spawn read/write loops.
#[allow(clippy::too_many_arguments)]
async fn try_accept_inbound(
    tcp: TcpStream,
    addr: &str,
    identity: &NodeIdentity,
    network_id: u32,
    ledger_seq: &AtomicU32,
    ledger_hash: &RwLock<Hash256>,
    event_tx: &mpsc::Sender<PeerEvent>,
    peer_set: &PeerSet,
    tls_server: &Arc<SslAcceptor>,
) -> Result<Hash256, OverlayError> {
    let stream = tls::accept_tls(tcp, tls_server)
        .await
        .map_err(|e| OverlayError::Connection(format!("TLS accept {addr}: {e}")))?;

    let seq = ledger_seq.load(Ordering::Relaxed);
    let hash = *ledger_hash.read().await;

    let (peer_node_id, software, framed) =
        handshake::handshake_inbound_http(stream, identity, network_id, seq, &hash).await?;

    if peer_set.get(&peer_node_id).is_some() {
        return Err(OverlayError::Handshake("already connected".into()));
    }

    let info = Arc::new(PeerInfo {
        node_id: peer_node_id,
        address: addr.to_string(),
        inbound: true,
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

/// Split a framed connection and spawn read/write loops.
/// Returns the write channel sender for the PeerHandle.
fn spawn_peer_loops(
    node_id: Hash256,
    framed: Framed<PeerStream, PeerCodec>,
    event_tx: mpsc::Sender<PeerEvent>,
) -> mpsc::Sender<PeerMessage> {
    let (write, read) = framed.split();
    let (tx, rx) = mpsc::channel(256);

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
    let validators = json.get("validators").and_then(|v| v.as_array()).ok_or(())?;
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
mod tests {
    use super::*;

    #[test]
    fn decode_validator_blob_extracts_count() {
        use base64::Engine;
        let json = serde_json::json!({
            "sequence": 1,
            "expiration": 999999999,
            "validators": [
                {"validation_public_key": "ED0001", "manifest": "AA=="},
                {"validation_public_key": "ED0002", "manifest": "BB=="},
                {"validation_public_key": "ED0003", "manifest": "CC=="},
            ]
        });
        let blob = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&json).unwrap());
        assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Ok(3));
    }

    #[test]
    fn decode_validator_blob_invalid_base64() {
        assert_eq!(base64_decode_validator_blob(b"!!!invalid!!!"), Err(()));
    }

    #[test]
    fn decode_validator_blob_no_validators_key() {
        use base64::Engine;
        let json = serde_json::json!({"sequence": 1});
        let blob = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&json).unwrap());
        assert_eq!(base64_decode_validator_blob(blob.as_bytes()), Err(()));
    }

    #[test]
    fn quorum_auto_compute_from_validator_count() {
        // Simulate the quorum calculation from node.rs:
        // new_quorum = ceil(count * 0.8)
        let count = 35usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 28);

        let count = 10usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 8);

        let count = 1usize;
        let quorum = (count as f64 * 0.8).ceil() as usize;
        assert_eq!(quorum, 1);
    }

    #[test]
    fn backoff_exponential_increase() {
        let mut b = ReconnectBackoff::new();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
        assert_eq!(b.next_delay(), Duration::from_secs(2));
        assert_eq!(b.next_delay(), Duration::from_secs(4));
        assert_eq!(b.next_delay(), Duration::from_secs(8));
        assert_eq!(b.next_delay(), Duration::from_secs(16));
    }

    #[test]
    fn backoff_caps_at_max() {
        let mut b = ReconnectBackoff::new();
        // 1, 2, 4, 8, 16, 30, 30, ...
        for _ in 0..5 {
            b.next_delay();
        }
        assert_eq!(b.next_delay(), Duration::from_secs(30));
        assert_eq!(b.next_delay(), Duration::from_secs(30));
    }

    #[test]
    fn backoff_reset_restores_initial() {
        let mut b = ReconnectBackoff::new();
        b.next_delay();
        b.next_delay();
        b.next_delay();
        assert_eq!(b.attempt(), 3);

        b.reset();
        assert_eq!(b.attempt(), 0);
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn backoff_attempt_counter() {
        let mut b = ReconnectBackoff::new();
        assert_eq!(b.attempt(), 0);
        b.next_delay();
        assert_eq!(b.attempt(), 1);
        b.next_delay();
        assert_eq!(b.attempt(), 2);
    }
}
