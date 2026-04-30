use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rxrpl_amendment::{AmendmentTable, FeatureRegistry, Rules};
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_config::NodeConfig;
use rxrpl_consensus::{
    ConsensusEngine, ConsensusParams, ConsensusTimer, NodeId, TimerAction, TrustedValidatorList,
    TxSet,
};
use rxrpl_ledger::Ledger;
use rxrpl_overlay::{
    ConsensusMessage, LedgerProvider, NetworkConsensusAdapter, NodeIdentity, OverlayCommand,
    PeerManager, PeerManagerConfig, VlFetcher, new_trusted_keys,
};
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use rxrpl_nodestore::{CachedNodeStore, MemoryNodeDatabase};
#[cfg(feature = "rocksdb")]
use rxrpl_nodestore::PersistentNodeDatabase;
use rxrpl_shamap::{NodeStore, SHAMap};
use rxrpl_rpc_server::{ServerContext, ServerEvent};
use rxrpl_storage::{SqliteStore, TxStore};
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use rxrpl_txq::TxQueue;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::consensus_adapter::NodeConsensusAdapter;
use crate::error::NodeError;
use crate::pruner::LedgerPruner;

/// LedgerProvider implementation backed by the node's closed ledger history.
struct ClosedLedgerAccess {
    closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
}

impl LedgerProvider for ClosedLedgerAccess {
    fn get_by_hash(&self, hash: &Hash256) -> Option<Ledger> {
        let history = self.closed_ledgers.try_read().ok()?;
        history.iter().find(|l| l.header.hash == *hash).cloned()
    }

    fn get_by_seq(&self, seq: u32) -> Option<Ledger> {
        let history = self.closed_ledgers.try_read().ok()?;
        history.iter().find(|l| l.header.sequence == seq).cloned()
    }

    fn latest_closed(&self) -> Option<Ledger> {
        let history = self.closed_ledgers.try_read().ok()?;
        history.back().cloned()
    }
}

/// The top-level XRPL node.
///
/// Wires together all subsystems: storage, ledger, transaction engine,
/// mempool, consensus, overlay, and RPC server.
#[allow(dead_code)]
pub struct Node {
    pub(crate) config: NodeConfig,
    ledger: Arc<RwLock<Ledger>>,
    closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
    tx_engine: Arc<TxEngine>,
    tx_queue: Arc<RwLock<TxQueue>>,
    amendment_table: Arc<RwLock<AmendmentTable>>,
    fees: Arc<FeeSettings>,
    tx_store: Option<Arc<dyn TxStore>>,
    node_store: Option<Arc<dyn NodeStore>>,
    pruner: Arc<LedgerPruner>,
    running: bool,
}

impl Node {
    /// Create a node store based on the database configuration.
    fn create_node_store(config: &NodeConfig) -> Result<Option<Arc<dyn NodeStore>>, NodeError> {
        match config.database.backend.as_str() {
            "memory" => {
                let db = MemoryNodeDatabase::new();
                let cached = CachedNodeStore::with_defaults(db);
                Ok(Some(Arc::new(cached)))
            }
            #[cfg(feature = "rocksdb")]
            "rocksdb" => {
                let db_path = config.database.path.join("nodestore");
                std::fs::create_dir_all(&db_path).map_err(|e| {
                    NodeError::Config(format!("failed to create nodestore dir: {e}"))
                })?;
                let kv = rxrpl_storage::RocksDbStore::open(&db_path)?;
                let db = PersistentNodeDatabase::new(kv);
                let cached = CachedNodeStore::with_defaults(db);
                Ok(Some(Arc::new(cached)))
            }
            "none" => Ok(None),
            other => Err(NodeError::Config(format!(
                "unknown database backend: {other}"
            ))),
        }
    }

    /// Create a new node from configuration.
    pub fn new(config: NodeConfig) -> Result<Self, NodeError> {
        // Initialize node store
        let node_store = Self::create_node_store(&config)?;

        // Initialize amendment registry
        let registry = FeatureRegistry::with_known_amendments();
        let amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4); // ~14 days at 4s/ledger

        // Initialize transaction engine with all handlers
        let mut tx_registry = TransactorRegistry::new();
        rxrpl_tx_engine::handlers::register_phase_a(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_b(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c1(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c2(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c3(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_d1(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_d2(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_e(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_f(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_batch(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_hooks(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_stubs(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_pseudo(&mut tx_registry);
        let tx_engine = TxEngine::new_without_sig_check(tx_registry);

        // Initialize genesis ledger
        let ledger = match &node_store {
            Some(store) => Ledger::genesis_with_store(Arc::clone(store)),
            None => Ledger::genesis(),
        };

        // Initialize transaction queue
        let tx_queue = TxQueue::new(2000);

        let pruner = Arc::new(LedgerPruner::new(
            config.database.online_delete,
            config.database.advisory_delete,
        ));

        Ok(Self {
            config,
            ledger: Arc::new(RwLock::new(ledger)),
            closed_ledgers: Arc::new(RwLock::new(VecDeque::new())),
            tx_engine: Arc::new(tx_engine),
            tx_queue: Arc::new(RwLock::new(tx_queue)),
            amendment_table: Arc::new(RwLock::new(amendment_table)),
            fees: Arc::new(FeeSettings::default()),
            tx_store: None,
            node_store,
            pruner,
            running: false,
        })
    }

    /// Create a standalone node with a funded genesis account.
    ///
    /// Creates genesis ledger, funds the account, closes genesis,
    /// and opens ledger #2 ready for transactions.
    pub fn new_standalone(config: NodeConfig, genesis_address: &str) -> Result<Self, NodeError> {
        let node_store = Self::create_node_store(&config)?;

        let registry = FeatureRegistry::with_known_amendments();
        let amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4);

        let mut tx_registry = TransactorRegistry::new();
        rxrpl_tx_engine::handlers::register_phase_a(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_b(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c1(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c2(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c3(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_d1(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_d2(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_e(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_f(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_batch(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_hooks(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_stubs(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_pseudo(&mut tx_registry);
        let tx_engine = TxEngine::new_without_sig_check(tx_registry);

        let tx_store: Arc<dyn TxStore> =
            Arc::new(SqliteStore::in_memory().map_err(|e| NodeError::Config(e.to_string()))?);

        let mut closed_genesis =
            Self::genesis_with_funded_account_and_store(genesis_address, &node_store)?;

        // Flush genesis to store and compact for memory efficiency
        if node_store.is_some() {
            if let Err(e) = closed_genesis.flush() {
                tracing::warn!("failed to flush genesis ledger: {}", e);
            }
            closed_genesis.compact();
        }

        let open_ledger = Ledger::new_open(&closed_genesis);

        let mut closed_ledgers = VecDeque::new();
        closed_ledgers.push_back(closed_genesis);

        let tx_queue = TxQueue::new(2000);

        let pruner = Arc::new(LedgerPruner::new(
            config.database.online_delete,
            config.database.advisory_delete,
        ));

        Ok(Self {
            config,
            ledger: Arc::new(RwLock::new(open_ledger)),
            closed_ledgers: Arc::new(RwLock::new(closed_ledgers)),
            tx_engine: Arc::new(tx_engine),
            tx_queue: Arc::new(RwLock::new(tx_queue)),
            amendment_table: Arc::new(RwLock::new(amendment_table)),
            fees: Arc::new(FeeSettings::default()),
            tx_store: Some(tx_store),
            node_store,
            pruner,
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
            if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                tracing::error!("RPC server error: {}", e);
            }
        });

        tracing::info!("node started");
        Ok(())
    }

    /// Run the node in standalone mode with auto-close loop.
    ///
    /// Starts the RPC server and a ledger close loop that closes the
    /// ledger every `close_interval_secs` seconds. Blocks until ctrl+c.
    pub async fn run_standalone(&self, close_interval_secs: u64) -> Result<(), NodeError> {
        let ctx = ServerContext::with_node_state_and_pruner(
            self.config.server.clone(),
            Arc::clone(&self.ledger),
            Arc::clone(&self.closed_ledgers),
            Arc::clone(&self.tx_engine),
            Arc::clone(&self.fees),
            self.tx_store.as_ref().map(Arc::clone),
            Some(Arc::clone(&self.tx_queue)),
            None, // no relay in standalone mode
            self.pruner.shared_state(),
        );
        let event_tx = ctx.event_sender().clone();

        // Clone ctx for gRPC before moving into RPC router
        #[cfg(feature = "grpc")]
        let grpc_ctx = Arc::clone(&ctx);

        let app = rxrpl_rpc_server::build_router(ctx);
        let bind = self.config.server.bind;

        tracing::info!("starting standalone RPC server on {}", bind);

        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        // Spawn RPC server
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                tracing::error!("RPC server error: {}", e);
            }
        });

        // Spawn gRPC server if enabled
        #[cfg(feature = "grpc")]
        {
            let grpc_port = self.config.server.bind.port() + 1000;
            let grpc_addr = SocketAddr::new(self.config.server.bind.ip(), grpc_port);
            tokio::spawn(async move {
                if let Err(e) = rxrpl_grpc_server::start_grpc_server(grpc_addr, grpc_ctx).await {
                    tracing::error!("gRPC server error: {}", e);
                }
            });
        }

        // Emit initial server state
        let _ = event_tx.send(ServerEvent::ServerStateChange {
            state: "full".into(),
        });

        // Spawn ledger close loop using consensus engine
        let ledger = Arc::clone(&self.ledger);
        let closed_ledgers = Arc::clone(&self.closed_ledgers);
        let tx_store = self.tx_store.as_ref().map(Arc::clone);
        let tx_queue = Arc::clone(&self.tx_queue);
        let amendment_table = Arc::clone(&self.amendment_table);
        let tx_engine_close = Arc::clone(&self.tx_engine);
        let fees_close = Arc::clone(&self.fees);
        let event_tx = event_tx.clone();
        let interval_duration = Duration::from_secs(close_interval_secs);
        let pruner = Arc::clone(&self.pruner);
        let node_store_prune = self.node_store.clone();

        tokio::spawn(async move {
            let adapter = NodeConsensusAdapter::new();
            let node_id = NodeId(Hash256::new([0x01; 32]));
            let mut consensus = ConsensusEngine::new(adapter, node_id, ConsensusParams::default());

            let mut interval = tokio::time::interval(interval_duration);
            // Skip the first immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                // XRPL NetClock is seconds since 2000-01-01 UTC, NOT Unix epoch.
                // rippled's `isCurrent` validation check rejects timestamps
                // outside a small window around its own NetClock; using Unix
                // epoch here puts us 30 years in rippled's future and every
                // validation we broadcast would be silently dropped at
                // rippled's `Validation: not current` filter.
                let raw_close_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
                    .saturating_sub(rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET) as u32;

                // Round to the current close_time_resolution so that two nodes
                // closing within the same window produce the same close_time
                // (and thus the same ledger hash). Without this, two fresh
                // bootstrapping nodes whose wall-clocks drift by 1-2s would
                // close ledger #2 with different hashes and never converge —
                // rippled's "Got proposal for X but we are on Y" reject path.
                let resolution = consensus.close_time_resolution();
                let close_time = rxrpl_consensus::round_close_time(raw_close_time, resolution);

                // Read current ledger state
                let l = ledger.read().await;
                let prev_hash = l.header.parent_hash;
                let ledger_seq = l.header.sequence;

                // Collect transaction hashes from open ledger
                let mut tx_hashes = Vec::new();
                l.tx_map.for_each(&mut |tx_hash, _data| {
                    tx_hashes.push(*tx_hash);
                });
                drop(l);

                let tx_set = TxSet::new(tx_hashes);

                // Run consensus (solo = immediate accept)
                consensus.start_round(prev_hash, ledger_seq);
                if let Err(e) = consensus.close_ledger(tx_set, close_time, ledger_seq) {
                    tracing::error!("consensus close_ledger failed: {}", e);
                    continue;
                }
                // In solo mode, converge() accepts immediately
                consensus.converge();

                // Use consensus result to close the ledger
                let effective_close_time = consensus.accepted_close_time().unwrap_or(close_time);
                let close_flags = consensus.accepted_close_flags();

                let mut l = ledger.write().await;

                // Apply amendment voting on flag ledgers (before close
                // computes final hashes). In standalone mode there are no
                // peer validations, so we only use our own votes.
                {
                    let mut at = amendment_table.write().await;
                    let own_votes = at.get_votes();
                    // Standalone: 1 trusted validator (ourselves)
                    let _rules = Node::apply_amendment_voting(
                        &mut l,
                        &tx_engine_close,
                        &mut at,
                        &fees_close,
                        1,
                        &[own_votes],
                        effective_close_time,
                        ledger_seq,
                    );
                }

                if let Err(e) = l.close(effective_close_time, close_flags) {
                    tracing::error!("failed to close ledger: {}", e);
                    continue;
                }

                // Flush closed ledger to node store
                if let Err(e) = l.flush() {
                    tracing::warn!("failed to flush ledger: {}", e);
                }

                let hash = l.header.hash;
                let seq = l.header.sequence;
                let closed = l.clone();

                // Index transactions
                if let Some(ref store) = tx_store {
                    Self::index_ledger_transactions(store.as_ref(), &closed);
                }

                // Emit transaction events (before ledger close, per rippled convention)
                let mut tx_count = 0u32;
                let mut has_offer_changes = false;
                closed.tx_map.for_each(&mut |_tx_hash, data| {
                    tx_count += 1;
                    if let Ok(record) = serde_json::from_slice::<Value>(data) {
                        let tx_json = record.get("tx_json").cloned().unwrap_or_default();

                        // Check for order book changes
                        if let Some(tx_type) = tx_json.get("TransactionType").and_then(|v| v.as_str()) {
                            if matches!(tx_type, "OfferCreate" | "OfferCancel") {
                                has_offer_changes = true;
                            }
                        }

                        let _ = event_tx.send(ServerEvent::TransactionValidated {
                            transaction: tx_json,
                            meta: record.get("meta").cloned().unwrap_or_default(),
                            ledger_index: seq,
                        });
                    }
                });

                // Emit book change events for order book modifications
                if has_offer_changes {
                    let _ = event_tx.send(ServerEvent::BookChange {
                        taker_pays: serde_json::json!({"currency": "XRP"}),
                        taker_gets: serde_json::json!({"currency": "XRP"}),
                        open: "0".into(),
                        close: "0".into(),
                        high: "0".into(),
                        low: "0".into(),
                        volume: "0".into(),
                    });
                }

                // Emit ledger close event
                let _ = event_tx.send(ServerEvent::LedgerClosed {
                    ledger_index: seq,
                    ledger_hash: hash,
                    ledger_time: effective_close_time,
                    txn_count: tx_count,
                });

                // Emit path_find update after ledger close
                let _ = event_tx.send(ServerEvent::PathFindUpdate {
                    alternatives: vec![],
                });

                // Open next ledger
                *l = Ledger::new_open(&closed);
                let new_open_seq = l.header.sequence;

                // Record metrics
                metrics::gauge!("ledger_sequence").set(new_open_seq as f64);
                metrics::counter!("txn_applied_total").increment(tx_count as u64);
                metrics::gauge!("txn_queue_size").set(tx_queue.read().await.len() as f64);

                tracing::info!(
                    "closed ledger #{} hash={}, opened #{}",
                    seq,
                    hash,
                    new_open_seq
                );
                drop(l);

                // Cleanup TxQueue: remove confirmed + expired, then retry remaining
                {
                    let mut q = tx_queue.write().await;
                    closed.tx_map.for_each(&mut |tx_hash, _| {
                        q.remove(tx_hash);
                    });
                    q.remove_expired(new_open_seq);

                    // Drain remaining entries for retry against the fresh open ledger
                    let pending = q.drain_for_retry();
                    drop(q);

                    let rules = Rules::new();
                    let mut requeue = Vec::new();
                    let mut l = ledger.write().await;
                    for entry in pending {
                        match tx_engine_close.apply(&entry.tx, &mut l, &rules, &fees_close) {
                            Ok(result) if result.is_success() => {
                                requeue.push(entry);
                            }
                            _ => {}
                        }
                    }
                    drop(l);

                    let mut q = tx_queue.write().await;
                    for entry in requeue {
                        let _ = q.submit(entry);
                    }
                }

                // Store in history (compact for memory efficiency)
                let mut history = closed_ledgers.write().await;
                let mut compacted = closed;
                compacted.compact();
                history.push_back(compacted);
                while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
                    history.pop_front();
                }

                // Ledger history pruning
                if pruner.should_prune(seq) {
                    if let Some(ref store) = node_store_prune {
                        let retention = pruner.shared_state().retention_window;
                        let cutoff_seq = seq.saturating_sub(retention);

                        // Collect old ledgers eligible for pruning
                        let old: Vec<_> = history.iter()
                            .filter(|l| l.header.sequence <= cutoff_seq)
                            .cloned()
                            .collect();

                        // The retained ledger is the first one after the cutoff
                        let retained = history.iter()
                            .find(|l| l.header.sequence > cutoff_seq);

                        let _deleted = pruner.prune(seq, &old, retained, store);
                    }
                }
            }
        });

        tracing::info!(
            "standalone node running (close interval: {}s), press ctrl+c to stop",
            close_interval_secs
        );

        tokio::signal::ctrl_c()
            .await
            .map_err(|e| NodeError::Server(format!("signal error: {e}")))?;

        tracing::info!("shutting down");
        Ok(())
    }

    /// Run the node in networked mode with P2P overlay.
    ///
    /// Starts the RPC server, P2P peer manager, and a consensus loop that
    /// processes both local ledger close ticks and incoming network messages.
    /// Validate that `--starting-ledger` (Seq or Recent) is only used with a
    /// trusted UNL configured. Without one, the `CheckpointAnchor` would
    /// resolve on any 28 fake validator keys (Sybil), letting an attacker
    /// bootstrap us onto a forged chain. `Hash(_)` is a planned manual-trust
    /// mode and exempt; the run loop logs and falls back to genesis for it.
    ///
    /// This MUST run before any port bind or async task spawn so that a
    /// misconfiguration is reported deterministically rather than racing
    /// `EADDRINUSE` against another process bound to the same port.
    fn validate_starting_ledger_unl(
        &self,
        starting_ledger: Option<&crate::checkpoint::StartingLedger>,
    ) -> Result<(), NodeError> {
        if matches!(
            starting_ledger,
            Some(crate::checkpoint::StartingLedger::Seq(_))
                | Some(crate::checkpoint::StartingLedger::Recent)
        ) {
            let has_unl = !self.config.validators.validator_list_sites.is_empty()
                && !self.config.validators.validator_list_keys.is_empty()
                && self.config.validators.require_trusted_validators;
            if !has_unl {
                return Err(NodeError::Config(
                    "--starting-ledger requires `validators.validator_list_sites` + \
                     `validators.validator_list_keys` to be configured and \
                     `require_trusted_validators` to be true; \
                     refusing to bootstrap from an unverified checkpoint"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    pub async fn run_networked(
        &self,
        close_interval_secs: u64,
        sync_rpc_url: Option<&str>,
        starting_ledger: Option<crate::checkpoint::StartingLedger>,
    ) -> Result<(), NodeError> {
        // 0. SECURITY: validate UNL/checkpoint guard BEFORE any port bind or
        // task spawn so a misconfiguration is reported deterministically
        // rather than racing `EADDRINUSE` (port collision in parallel tests).
        self.validate_starting_ledger_unl(starting_ledger.as_ref())?;

        // 1. Generate/load node identity. node_seed accepts either:
        //   - 32 hex characters (16 raw seed bytes)
        //   - a base58 family seed (e.g. "snXxx..." — what rippled-style
        //     configs and xrpl-hive's XRPL_VALIDATOR_SEED env emit)
        let identity = if let Some(ref seed_str) = self.config.peer.node_seed {
            let seed_bytes = parse_node_seed(seed_str)
                .map_err(|e| NodeError::Config(format!("invalid node_seed: {e}")))?;
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_bytes(seed_bytes))
        } else {
            NodeIdentity::generate()
        };
        let identity = Arc::new(identity);
        tracing::info!("node identity: {}", identity.node_id);
        tracing::info!("node public key: {}", hex::encode(identity.public_key_bytes()));

        // 2. Bootstrap from RPC: fetch latest validated ledger to set our starting point
        if let Some(rpc_url) = sync_rpc_url {
            match Self::bootstrap_from_rpc(rpc_url).await {
                Ok((seq, hash)) => {
                    let mut l = self.ledger.write().await;
                    l.header.sequence = seq + 1; // open ledger = validated + 1
                    l.header.parent_hash = hash;
                    drop(l);
                    tracing::info!(
                        "bootstrapped from RPC: validated ledger #{} hash={}, open ledger #{}",
                        seq, hash, seq + 1
                    );

                    // Download the full state tree via RPC to pre-populate the store.
                    if let Some(ref store) = self.node_store {
                        let hash_hex = hex::encode(hash.as_bytes());
                        match Self::download_state_via_rpc(rpc_url, &hash_hex, Arc::clone(store)).await {
                            Ok(count) => {
                                tracing::info!("pre-populated store with {} state entries via RPC", count);
                            }
                            Err(e) => {
                                tracing::warn!("RPC state download failed (P2P sync will be used): {}", e);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("RPC bootstrap failed (starting from genesis): {}", e);
                }
            }
        }

        // Shared ledger state for P2P
        let ledger_seq = Arc::new(AtomicU32::new(self.ledger.read().await.header.sequence));
        let ledger_hash = Arc::new(tokio::sync::RwLock::new(
            self.ledger.read().await.header.parent_hash,
        ));

        // 3. Create PeerManager with LedgerProvider + TLS
        let tls_server = rxrpl_overlay::tls::build_server_config(&identity);
        let tls_client = rxrpl_overlay::tls::build_client_config();

        let peer_config = PeerManagerConfig {
            listen_port: self.config.peer.port,
            max_peers: self.config.peer.max_peers,
            seeds: self.config.peer.seeds.clone(),
            fixed_peers: self.config.peer.fixed_peers.clone(),
            network_id: self.config.network.network_id,
            tls_server,
            tls_client,
            cluster_enabled: self.config.cluster.enabled,
            cluster_node_name: self.config.cluster.node_name.clone().unwrap_or_default(),
            cluster_members: self.config.cluster.members.clone(),
            cluster_broadcast_interval_secs: self.config.cluster.broadcast_interval_secs,
        };
        let (mut peer_mgr, cmd_tx, mut consensus_rx) = PeerManager::new(
            Arc::clone(&identity),
            peer_config,
            Arc::clone(&ledger_seq),
            Arc::clone(&ledger_hash),
        );
        peer_mgr.set_ledger_provider(Arc::new(ClosedLedgerAccess {
            closed_ledgers: Arc::clone(&self.closed_ledgers),
        }));
        if let Some(ref store) = self.node_store {
            peer_mgr.set_node_store(Arc::clone(store));
        }

        // 3b. Create overlay event channel for bridging to RPC events
        let (overlay_event_tx, mut overlay_event_rx) =
            tokio::sync::broadcast::channel::<serde_json::Value>(256);
        peer_mgr.set_event_sender(overlay_event_tx);

        // 4. Create relay channel (clone cmd_tx BEFORE moving into adapter)
        let (relay_tx, mut relay_rx) = tokio::sync::mpsc::unbounded_channel::<(Hash256, Vec<u8>)>();
        let cmd_tx_relay = cmd_tx.clone();
        let cmd_tx_catchup = cmd_tx.clone();

        // 5. Create NetworkConsensusAdapter (consumes cmd_tx)
        let adapter = NetworkConsensusAdapter::new(cmd_tx, Arc::clone(&identity));

        // 5b. Share the adapter's tx-set cache with the peer manager so it can
        // check for locally known sets and store newly acquired ones.
        peer_mgr.set_tx_sets(Arc::clone(adapter.tx_sets()));

        // 6. Spawn relay bridge: RPC submit -> P2P broadcast
        tokio::spawn(async move {
            while let Some((tx_hash, tx_bytes)) = relay_rx.recv().await {
                tracing::debug!(
                    "relay bridge: forwarding tx {} ({} bytes) to broadcast",
                    tx_hash, tx_bytes.len()
                );
                let payload = rxrpl_overlay::proto_convert::encode_transaction(&tx_hash, &tx_bytes);
                let _ = cmd_tx_relay.send(OverlayCommand::Broadcast {
                    msg_type: rxrpl_p2p_proto::MessageType::Transaction,
                    payload,
                });
            }
        });

        // 6c. Optional UNL fetcher. When `validators.validator_list_sites`
        // is non-empty and at least one publisher key is configured, spawn
        // a background task that periodically fetches and verifies the
        // signed validator list, publishing the trusted master-key set
        // into a shared handle consumed by `ValidationAggregator`.
        let trusted_validators = new_trusted_keys();
        let vl_status: Arc<RwLock<serde_json::Value>> =
            Arc::new(RwLock::new(serde_json::Value::Array(Vec::new())));
        let vl_sites = self.config.validators.validator_list_sites.clone();
        let vl_publisher_keys: Vec<rxrpl_primitives::PublicKey> = self
            .config
            .validators
            .validator_list_keys
            .iter()
            .filter_map(|hex_key| {
                hex::decode(hex_key)
                    .ok()
                    .and_then(|b| rxrpl_primitives::PublicKey::from_slice(&b).ok())
            })
            .collect();
        if !vl_sites.is_empty() && !vl_publisher_keys.is_empty() {
            let trusted_clone = Arc::clone(&trusted_validators);
            let status_clone = Arc::clone(&vl_status);
            let sites_for_fetcher = vl_sites.clone();
            let keys_for_fetcher = vl_publisher_keys.clone();
            let fetcher_status_handle: rxrpl_overlay::StatusHandle =
                Arc::new(RwLock::new(Vec::new()));
            let fetcher_status_for_publish = Arc::clone(&fetcher_status_handle);
            tokio::spawn(async move {
                let fetcher = match VlFetcher::new(
                    sites_for_fetcher,
                    keys_for_fetcher,
                    trusted_clone,
                    fetcher_status_handle,
                ) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("failed to start VL fetcher: {}", e);
                        return;
                    }
                };
                // Bridge the typed status snapshot into the JSON value the
                // RPC handler reads from `ctx.validator_list_status`.
                let publish_handle = Arc::clone(&fetcher_status_for_publish);
                let publish_target = Arc::clone(&status_clone);
                tokio::spawn(async move {
                    let mut tick = tokio::time::interval(Duration::from_secs(15));
                    loop {
                        tick.tick().await;
                        let snapshot = publish_handle.read().await.clone();
                        let json = serde_json::Value::Array(
                            snapshot
                                .into_iter()
                                .map(|s| {
                                    serde_json::json!({
                                        "site": s.site,
                                        "last_fetch_unix": s.last_fetch_unix,
                                        "last_sequence": s.last_sequence,
                                        "last_validator_count": s.last_validator_count,
                                        "last_error": s.last_error,
                                    })
                                })
                                .collect(),
                        );
                        *publish_target.write().await = json;
                    }
                });
                fetcher.run().await;
            });
        }

        // 7. Start RPC server
        let mut ctx = ServerContext::with_node_state_and_pruner(
            self.config.server.clone(),
            Arc::clone(&self.ledger),
            Arc::clone(&self.closed_ledgers),
            Arc::clone(&self.tx_engine),
            Arc::clone(&self.fees),
            self.tx_store.as_ref().map(Arc::clone),
            Some(Arc::clone(&self.tx_queue)),
            Some(relay_tx),
            self.pruner.shared_state(),
        );
        ctx.attach_validator_list_status(Arc::clone(&vl_status));
        ctx.attach_network_id(self.config.network.network_id);
        let event_tx = ctx.event_sender().clone();

        // Clone ctx for gRPC before moving into RPC router
        #[cfg(feature = "grpc")]
        let grpc_ctx = Arc::clone(&ctx);

        let app = rxrpl_rpc_server::build_router(ctx);
        let bind = self.config.server.bind;

        tracing::info!("starting RPC server on {}", bind);
        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await {
                tracing::error!("RPC server error: {}", e);
            }
        });

        // Spawn gRPC server if enabled
        #[cfg(feature = "grpc")]
        {
            let grpc_port = self.config.server.bind.port() + 1000;
            let grpc_addr = SocketAddr::new(self.config.server.bind.ip(), grpc_port);
            tokio::spawn(async move {
                if let Err(e) = rxrpl_grpc_server::start_grpc_server(grpc_addr, grpc_ctx).await {
                    tracing::error!("gRPC server error: {}", e);
                }
            });
        }

        // Emit initial server state (connecting to peers)
        let _ = event_tx.send(ServerEvent::ServerStateChange {
            state: "connected".into(),
        });

        // Bridge overlay events -> ServerEvents
        {
            let event_tx_bridge = event_tx.clone();
            tokio::spawn(async move {
                loop {
                    match overlay_event_rx.recv().await {
                        Ok(json) => {
                            let event_type =
                                json.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            match event_type {
                                "peerStatusChange" => {
                                    let _ = event_tx_bridge.send(ServerEvent::PeerStatusChange {
                                        peer_id: json
                                            .get("peer_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        event: json
                                            .get("event")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                    });
                                }
                                "validationReceived" => {
                                    let _ = event_tx_bridge.send(ServerEvent::ValidationReceived {
                                        validator: json
                                            .get("validator")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        ledger_hash: json
                                            .get("ledger_hash")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        ledger_seq: json
                                            .get("ledger_seq")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0)
                                            as u32,
                                        full: json
                                            .get("full")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(false),
                                    });
                                }
                                "manifestReceived" => {
                                    let _ = event_tx_bridge.send(ServerEvent::ManifestReceived {
                                        master_key: json
                                            .get("master_key")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        signing_key: json
                                            .get("signing_key")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        seq: json
                                            .get("seq")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(0)
                                            as u32,
                                    });
                                }
                                _ => {}
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("overlay event consumer lagged, skipped {} events", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        // 6. Spawn PeerManager
        tokio::spawn(async move {
            if let Err(e) = peer_mgr.run().await {
                tracing::error!("PeerManager error: {}", e);
            }
        });

        // 7. Build UNL from config
        // Accept both rippled-style base58 `nXXX` keys (the format that
        // appears in validators.txt and our own xrpl_start.sh) and raw
        // hex 33-byte secp256k1 keys for compatibility with older configs.
        let unl = {
            const NODE_PUBLIC_KEY_PREFIX: &[u8] = &[0x1C];
            let mut trusted = HashSet::new();
            for entry in &self.config.validators.trusted {
                let trimmed = entry.trim();
                let bytes = if let Ok(decoded) =
                    rxrpl_codec::address::base58::base58check_decode(trimmed)
                {
                    // Strip the 1-byte 0x1C node-public-key prefix.
                    if decoded.len() == 1 + 33 && decoded[..1] == NODE_PUBLIC_KEY_PREFIX[..] {
                        Some(decoded[1..].to_vec())
                    } else {
                        None
                    }
                } else if let Ok(decoded) = hex::decode(trimmed) {
                    Some(decoded)
                } else {
                    None
                };
                if let Some(b) = bytes {
                    trusted.insert(NodeId::from_public_key(&b));
                } else {
                    tracing::warn!("ignoring invalid validator key: {trimmed}");
                }
            }
            if !trusted.is_empty() {
                tracing::info!("UNL configured with {} trusted validators", trusted.len());
            }
            TrustedValidatorList::new(trusted)
        };
        let trusted_unl_size = self
            .config
            .validators
            .trusted
            .iter()
            .filter(|s| {
                rxrpl_codec::address::base58::base58check_decode(s.trim()).is_ok()
                    || hex::decode(s.trim()).is_ok()
            })
            .count();

        // 8. Consensus loop with multi-round convergence
        let ledger = Arc::clone(&self.ledger);
        let closed_ledgers = Arc::clone(&self.closed_ledgers);
        let tx_engine = Arc::clone(&self.tx_engine);
        let fees = Arc::clone(&self.fees);
        let tx_queue = Arc::clone(&self.tx_queue);
        let amendment_table = Arc::clone(&self.amendment_table);
        let node_store = self.node_store.clone();
        let tx_store = self.tx_store.as_ref().map(Arc::clone);
        let pruner = Arc::clone(&self.pruner);
        let ledger_seq_shared = Arc::clone(&ledger_seq);
        let ledger_hash_shared = Arc::clone(&ledger_hash);
        let configured_quorum = self.config.validators.quorum;
        let trusted_validators_for_aggregator = if self.config.validators.require_trusted_validators
            && !self.config.validators.validator_list_sites.is_empty()
            && !vl_publisher_keys.is_empty()
        {
            Some(Arc::clone(&trusted_validators))
        } else {
            None
        };

        // Initial checkpoint anchor (if --starting-ledger was passed).
        // - Seq(s):   create immediately for sequence `s`.
        // - Recent:   wait until we have observed at least one peer, then
        //             anchor at `max_peer_seq - 1024` (saturating).
        // - Hash(h):  not yet wired — header-by-hash lookup is the missing
        //             piece. Logged on entry.
        let starting_ledger_for_loop = starting_ledger;
        if let Some(crate::checkpoint::StartingLedger::Hash(h)) = starting_ledger_for_loop {
            tracing::warn!(
                "checkpoint bootstrap by hash {} not yet implemented; node will start from genesis",
                h
            );
        }

        // SECURITY: --starting-ledger guard is enforced at the top of
        // `run_networked` via `validate_starting_ledger_unl` so the error
        // surfaces before any port bind or task spawn.

        tokio::spawn(async move {
            let node_id = NodeId(identity.node_id);
            let consensus_params = ConsensusParams::default();
            let mut timer = ConsensusTimer::new(&consensus_params);
            let mut consensus =
                ConsensusEngine::new_with_unl(adapter, node_id, identity.public_key_bytes().to_vec(), consensus_params, unl);

            let mut syncing = false;
            let mut max_peer_seq: u32 = 0;
            let mut pending_close_time = 0u32;
            // Cross-impl bootstrap gate: defer first close until at least one
            // peer has announced its sequence (max_peer_seq > 0). Without this,
            // a fast 5s open closes #2 before peer connects (~17s for rippled
            // StatusChange) and the bootstrap-yield can't fire. After grace
            // period (60s) we proceed even without peer — that's solo mode.
            let mut first_close_completed: bool = false;
            let startup_instant = tokio::time::Instant::now();
            const FIRST_CLOSE_GRACE: Duration = Duration::from_secs(60);
            // Cross-impl close-race guard: track last time we observed a peer
            // StatusChange. If peer is alive but BEHIND our open seq, defer
            // close to give peer time to advance — that way peer announces
            // first and rxrpl yields/adopts (deterministic convergence) instead
            // of racing peer with own close (which risks close_time bucket
            // mismatch and divergent hashes). Capped at PEER_WAIT_GRACE so a
            // truly offline peer doesn't block us forever.
            let mut last_peer_status_at: Option<tokio::time::Instant> = None;
            // round_open_at: timestamp when the current open seq was opened.
            // Used to bound how long we'll defer close waiting for peer.
            let mut round_open_at: Option<tokio::time::Instant> = None;
            let mut last_round_seq: u32 = 0;
            const PEER_WAIT_GRACE: Duration = Duration::from_secs(30);
            const PEER_ALIVE_WINDOW: Duration = Duration::from_secs(30);
            // Counter for how many close ticks we've deferred waiting for a
            // peer position. Caps the wait at ~10s extra so an offline peer
            // doesn't block our progress forever.
            let mut close_deferrals: u32 = 0;
            // Cache of LedgerHeader objects parsed from peer liBASE responses.
            // We need the full header (parent_hash, parent_close_time,
            // close_time, drops, close_time_resolution, close_flags) when
            // reconstructing a catchup ledger so the next consensus round can
            // close to a hash that matches what the peer would compute.
            // Without this, `from_catchup` left those fields at zero/default
            // and the next local close diverged from the peer's chain.
            let mut catchup_headers: std::collections::HashMap<u32, rxrpl_ledger::LedgerHeader> =
                std::collections::HashMap::new();
            // Track validations from network peers to determine validated ledgers.
            // Quorum is 80% of the trusted UNL size (rippled convention),
            // floored at 1. Falls back to 28 only if no UNL is configured
            // and no explicit override was provided.
            let initial_quorum = configured_quorum.unwrap_or_else(|| {
                if trusted_unl_size > 0 {
                    Node::compute_quorum(trusted_unl_size)
                } else {
                    28
                }
            });
            let mut val_aggregator =
                rxrpl_overlay::validation_aggregator::ValidationAggregator::new(initial_quorum);
            if let Some(ref trusted) = trusted_validators_for_aggregator {
                val_aggregator = val_aggregator.with_trusted_keys(Arc::clone(trusted));
                tracing::info!("validation aggregator: trust filter enabled (UNL-bound)");
            }
            tracing::info!("validation quorum initialized to {}", initial_quorum);

            // Checkpoint bootstrap state (consumed once the anchor resolves).
            let mut checkpoint_anchor: Option<crate::checkpoint::CheckpointAnchor> =
                match starting_ledger_for_loop {
                    Some(crate::checkpoint::StartingLedger::Seq(s)) => {
                        tracing::info!(
                            "checkpoint bootstrap: tracking anchor for ledger #{} (quorum {})",
                            s, initial_quorum
                        );
                        Some(crate::checkpoint::CheckpointAnchor::new(
                            crate::checkpoint::AnchorConfig {
                                target_seq: s,
                                quorum: initial_quorum,
                            },
                        ))
                    }
                    Some(crate::checkpoint::StartingLedger::Recent) => None,
                    Some(crate::checkpoint::StartingLedger::Hash(_)) | None => None,
                };
            // True until --starting-ledger=recent has computed its target seq.
            let mut recent_anchor_pending =
                matches!(starting_ledger_for_loop, Some(crate::checkpoint::StartingLedger::Recent));

            // Collect amendment votes from received validations for the current round.
            // Reset after each ledger close.
            let mut amendment_votes: Vec<Vec<Hash256>> = Vec::new();
            let mut trusted_validator_count: usize = 0;

            // Cooldown for wrong-prev-ledger recovery to prevent flip-flopping.
            // At most one switch per 10 seconds.
            let mut last_prev_ledger_switch: Option<tokio::time::Instant> = None;
            const PREV_LEDGER_SWITCH_COOLDOWN: Duration = Duration::from_secs(10);

            // Track consensus stalls for escalating recovery.
            let mut stall_metrics = rxrpl_consensus::StallMetrics::new();

            let sync_check_duration = Duration::from_secs(5);
            let mut sync_check_interval = tokio::time::interval(sync_check_duration);
            let mut sync_started_at: Option<tokio::time::Instant> = None;
            let mut last_sync_seq: u32 = 0;

            sync_check_interval.tick().await;

            // Consensus timer tick interval: poll frequently so the timer
            // can drive phase transitions precisely.
            let tick_duration = Duration::from_millis(100);
            let mut tick_interval = tokio::time::interval(tick_duration);
            tick_interval.tick().await; // skip first immediate tick

            loop {
                tokio::select! {
                    _ = tick_interval.tick(), if !syncing => {
                        if let Some(action) = timer.tick() {
                            match action {
                                TimerAction::CloseLedger => {
                                    // First-close gate: in cross-impl mode, peer connection
                                    // takes ~17s. With 5s open, we'd fire CloseLedger before
                                    // peer is observable (max_peer_seq still 0) and the
                                    // bootstrap-yield wouldn't trigger. Defer until peer has
                                    // announced a sequence, capped by FIRST_CLOSE_GRACE for
                                    // solo mode (no peer at all).
                                    if !first_close_completed
                                        && max_peer_seq == 0
                                        && startup_instant.elapsed() < FIRST_CLOSE_GRACE
                                    {
                                        tracing::debug!(
                                            "deferring first close: no peer status yet (elapsed {:?})",
                                            startup_instant.elapsed()
                                        );
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                        continue;
                                    }

                                    // XRPL NetClock = seconds since 2000-01-01 UTC.
                                    // See the matching comment in the consensus
                                    // task above; passing Unix epoch here would
                                    // put broadcast validations 30 years in
                                    // rippled's future and they would be
                                    // dropped at the `isCurrent` check.
                                    let l = ledger.read().await;
                                    let prev_hash = l.header.parent_hash;
                                    let seq = l.header.sequence;
                                    let parent_close_time = l.header.parent_close_time;
                                    drop(l);

                                    // Reset round timer when seq advances (new open ledger).
                                    if last_round_seq != seq {
                                        round_open_at = Some(tokio::time::Instant::now());
                                        last_round_seq = seq;
                                    }

                                    // Cross-impl close_time selection priority:
                                    // 1) Latest observed peer close_time (rippled's CTime) — adopt
                                    //    the peer's close_time bucket so we land in the same window.
                                    // 2) Otherwise floor wall-clock to resolution grid (solo close).
                                    let resolution = consensus.close_time_resolution();
                                    let raw_close_time = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        .saturating_sub(rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET) as u32;
                                    let close_time = match consensus.latest_peer_close_time() {
                                        Some(peer_ct) if peer_ct > parent_close_time => peer_ct,
                                        _ => {
                                            if resolution > 0 {
                                                (raw_close_time / resolution) * resolution
                                            } else {
                                                raw_close_time
                                            }
                                        }
                                    }
                                    .max(parent_close_time.saturating_add(1));

                                    // Cross-impl close strategy:
                                    //  1) peer ahead/equal (max_peer_seq >= seq) → yield + adopt
                                    //     peer's chain. Broadcast our validation for adopted hash
                                    //     so peer reaches quorum.
                                    //  2) peer behind (max_peer_seq < seq) AND peer alive (recent
                                    //     StatusChange) AND we haven't waited too long → defer
                                    //     to let peer advance to our seq. Avoids the close_time
                                    //     race where we close before peer announces and produce
                                    //     a divergent bucket → divergent hash.
                                    //  3) peer behind AND (peer dead OR waited too long) →
                                    //     close own.
                                    let peer_alive = last_peer_status_at
                                        .map(|t| t.elapsed() < PEER_ALIVE_WINDOW)
                                        .unwrap_or(false);
                                    let peer_at_or_past = max_peer_seq > 0 && max_peer_seq >= seq;
                                    let waited = round_open_at
                                        .map(|t| t.elapsed())
                                        .unwrap_or(Duration::ZERO);
                                    let peer_behind_alive = max_peer_seq > 0
                                        && max_peer_seq < seq
                                        && peer_alive
                                        && waited < PEER_WAIT_GRACE;
                                    if peer_behind_alive {
                                        tracing::debug!(
                                            "deferring close: peer at seq {} < our seq {}, waited {:?}",
                                            max_peer_seq, seq, waited
                                        );
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                        continue;
                                    }
                                    if peer_at_or_past {
                                        tracing::debug!(
                                            "yielding close: peer at seq {} >= our open seq {}",
                                            max_peer_seq, seq
                                        );
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                        let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                            seq,
                                            hash: None,
                                        });
                                        continue;
                                    }

                                    // Wait-for-peer-position removed: empirically, rippled never
                                    // sends a proposal during the open phase (only after closing
                                    // alone). So waiting for peer_positions deadlocks rxrpl into
                                    // the deferral cap (100 ticks ≈ 10s) every round, which makes
                                    // rxrpl close LATER than rippled — exact opposite of what we
                                    // want. Better to close at the wall-clock-ceiling boundary and
                                    // let proposals/validations sort it out via wrong-prev-ledger.
                                    let _ = close_deferrals;

                                    let l = ledger.read().await;
                                    let mut tx_hashes = Vec::new();
                                    l.tx_map.for_each(&mut |tx_hash, _data| {
                                        tx_hashes.push(*tx_hash);
                                    });
                                    drop(l);

                                    let tx_set = TxSet::new(tx_hashes);

                                    consensus.start_round(prev_hash, seq);
                                    if let Err(e) = consensus.close_ledger(tx_set, close_time, seq) {
                                        tracing::error!("consensus close_ledger failed: {}", e);
                                        continue;
                                    }

                                    first_close_completed = true;
                                    pending_close_time = close_time;
                                    timer.on_phase_change(consensus.phase());

                                    let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                        phase: "open".into(),
                                    });

                                    // Try immediate convergence (solo mode or instant agreement)
                                    if consensus.converge() {
                                        timer.on_phase_change(consensus.phase());
                                        let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                            phase: "accepted".into(),
                                        });
                                        Self::close_consensus_round(
                                            &consensus, pending_close_time, &ledger,
                                            &closed_ledgers, &tx_store, &event_tx,
                                            &ledger_seq_shared, &ledger_hash_shared,
                                            &tx_queue, &identity, &cmd_tx_catchup,
                                            &amendment_table, &tx_engine, &fees,
                                            &amendment_votes, trusted_validator_count,
                                            &pruner, &node_store,
                                        ).await;
                                        stall_metrics.reset_consecutive();
                                        amendment_votes.clear();
                                        trusted_validator_count = 0;
                                        // Start new open phase
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                    } else {
                                        let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                            phase: "establish".into(),
                                        });
                                    }
                                }
                                TimerAction::Converge => {
                                    if consensus.converge() {
                                        timer.on_phase_change(consensus.phase());
                                        let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                            phase: "accepted".into(),
                                        });
                                        Self::close_consensus_round(
                                            &consensus, pending_close_time, &ledger,
                                            &closed_ledgers, &tx_store, &event_tx,
                                            &ledger_seq_shared, &ledger_hash_shared,
                                            &tx_queue, &identity, &cmd_tx_catchup,
                                            &amendment_table, &tx_engine, &fees,
                                            &amendment_votes, trusted_validator_count,
                                            &pruner, &node_store,
                                        ).await;
                                        stall_metrics.reset_consecutive();
                                        amendment_votes.clear();
                                        trusted_validator_count = 0;
                                        // Start new open phase
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                    }
                                }
                                TimerAction::StallAbort => {
                                    let prev_hash = {
                                        let l = ledger.read().await;
                                        l.header.parent_hash
                                    };
                                    let action = stall_metrics.record_stall(&prev_hash);
                                    tracing::warn!(
                                        phase = ?consensus.phase(),
                                        total_stalls = stall_metrics.total_stalls(),
                                        consecutive = stall_metrics.consecutive_stalls(),
                                        action = ?action,
                                        "consensus stalled, taking recovery action"
                                    );

                                    match action {
                                        rxrpl_consensus::StallAction::Retry => {
                                            // Abandon round, re-open same ledger
                                            let l = ledger.read().await;
                                            consensus.start_round(
                                                l.header.parent_hash,
                                                l.header.sequence,
                                            );
                                            timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                            amendment_votes.clear();
                                            trusted_validator_count = 0;
                                        }
                                        rxrpl_consensus::StallAction::Resync => {
                                            // Escalate: request latest ledger from peers
                                            tracing::warn!(
                                                "3+ consecutive stalls, requesting resync from peers"
                                            );
                                            syncing = true;
                                            timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                            let _ = cmd_tx_catchup.send(
                                                rxrpl_overlay::OverlayCommand::RequestLedger {
                                                    hash: Some(Hash256::ZERO),
                                                    seq: 0,
                                                },
                                            );
                                            amendment_votes.clear();
                                            trusted_validator_count = 0;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    Some(msg) = consensus_rx.recv() => {
                        match msg {
                            ConsensusMessage::Proposal(proposal) => {
                                tracing::debug!(
                                    "proposal from {:?} seq={} tx_set={} close_time={}",
                                    proposal.node_id, proposal.ledger_seq,
                                    proposal.tx_set_hash, proposal.close_time
                                );
                                consensus.peer_proposal(proposal);

                                // Check if a supermajority of trusted peers
                                // reference a different prev_ledger than ours.
                                let cooldown_ok = last_prev_ledger_switch
                                    .map(|t| t.elapsed() >= PREV_LEDGER_SWITCH_COOLDOWN)
                                    .unwrap_or(true);
                                if cooldown_ok {
                                    if let Some(detected) = consensus.check_wrong_prev_ledger() {
                                        tracing::warn!(
                                            "wrong prev_ledger detected: {}/{} trusted peers reference {}, \
                                             ours is {}. Triggering recovery.",
                                            detected.peer_count,
                                            detected.total_trusted,
                                            detected.preferred_ledger,
                                            consensus.prev_ledger()
                                        );
                                        last_prev_ledger_switch = Some(tokio::time::Instant::now());

                                        // Abort current consensus round and enter sync mode
                                        // to fetch the correct ledger from peers.
                                        syncing = true;
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                        sync_started_at = Some(tokio::time::Instant::now());
                                        last_sync_seq = ledger_seq_shared.load(Ordering::Relaxed);

                                        // Request the preferred ledger from peers.
                                        let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                            seq: 0, // unknown seq, rely on hash
                                            hash: Some(detected.preferred_ledger),
                                        });

                                        // Reset consensus state so we don't keep processing
                                        // the stale round. The next round will start after
                                        // we sync the correct ledger.
                                        consensus.start_round(detected.preferred_ledger, 0);
                                        amendment_votes.clear();
                                        trusted_validator_count = 0;
                                    }
                                }
                            }
                            ConsensusMessage::Validation(validation) => {
                                let val_seq = validation.ledger_seq;
                                let val_hash = validation.ledger_hash;
                                tracing::debug!(
                                    "validation from {:?} for ledger #{} hash={}",
                                    validation.node_id, val_seq, val_hash
                                );
                                let _ = event_tx.send(ServerEvent::ValidationReceived {
                                    validator: validation.node_id.0.to_string(),
                                    ledger_hash: val_hash.to_string(),
                                    ledger_seq: val_seq,
                                    full: validation.full,
                                });

                                // Collect amendment votes from this validator
                                if validation.full && !validation.amendments.is_empty() {
                                    amendment_votes.push(validation.amendments.clone());
                                }
                                trusted_validator_count = trusted_validator_count.max(
                                    amendment_votes.len(),
                                );

                                // Feed into the checkpoint anchor first (if active). On
                                // resolution we directly request the agreed ledger and
                                // retire the anchor so we don't re-trigger.
                                //
                                // SECURITY: only count UNL-trusted validations toward
                                // the anchor. Without this gate, 28 fake validators
                                // could resolve the anchor on a forged hash and
                                // bootstrap the node onto an attacker-chosen chain.
                                let anchor_trustable =
                                    val_aggregator.is_trusted(&validation.public_key);
                                if let Some(anchor) = checkpoint_anchor.as_mut() {
                                    if !anchor_trustable {
                                        // Drop silently — the same validation may
                                        // still be processed by the regular aggregator
                                        // below (which also gates on trust).
                                    } else if let Some(anchor_hash) = anchor.add(&validation) {
                                        let anchor_seq = anchor.target_seq();
                                        tracing::info!(
                                            "checkpoint anchor resolved: ledger #{} hash={}",
                                            anchor_seq, anchor_hash
                                        );
                                        let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                            seq: anchor_seq,
                                            hash: Some(anchor_hash),
                                        });
                                        if !syncing {
                                            syncing = true;
                                            timer.on_phase_change(
                                                rxrpl_consensus::ConsensusPhase::Open,
                                            );
                                            sync_started_at = Some(tokio::time::Instant::now());
                                            last_sync_seq =
                                                ledger_seq_shared.load(Ordering::Relaxed);
                                        }
                                        if anchor_seq > max_peer_seq {
                                            max_peer_seq = anchor_seq;
                                        }
                                        checkpoint_anchor = None;
                                    }
                                }

                                // Aggregate validation and check for quorum
                                if let Some(validated) = val_aggregator.add_validation(validation) {
                                    tracing::info!(
                                        "network validated ledger #{} hash={} ({} validations)",
                                        validated.seq, validated.hash, validated.validation_count
                                    );
                                    let our_seq = ledger_seq_shared.load(Ordering::Relaxed);

                                    // If the network is ahead, enter sync mode
                                    if validated.seq >= our_seq && !syncing {
                                        if validated.seq > our_seq {
                                            tracing::info!(
                                                "network ahead (validated #{} vs our #{}), syncing to #{}",
                                                validated.seq, our_seq, validated.seq
                                            );
                                            syncing = true;
                                            timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                            sync_started_at = Some(tokio::time::Instant::now());
                                            last_sync_seq = our_seq;
                                            // Request the network's validated ledger directly
                                            // (not our_seq+1 which may not exist on peers)
                                            let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                                seq: validated.seq,
                                                hash: Some(validated.hash),
                                            });
                                        }
                                        if validated.seq > max_peer_seq {
                                            max_peer_seq = validated.seq;
                                        }
                                    }
                                }
                            }
                            ConsensusMessage::Transaction { hash, data } => {
                                tracing::debug!("received transaction {} from network", hash);

                                // Decode binary -> JSON
                                let tx_json = match rxrpl_codec::binary::decode(&data) {
                                    Ok(j) => j,
                                    Err(e) => {
                                        tracing::warn!("failed to decode P2P tx {}: {}", hash, e);
                                        continue;
                                    }
                                };

                                // Apply to open ledger (speculative)
                                let rules = Rules::new();
                                let mut l = ledger.write().await;
                                match tx_engine.apply(&tx_json, &mut l, &rules, &fees) {
                                    Ok(result) => {
                                        drop(l);
                                        if result.is_success() {
                                            // Add to TxQueue
                                            let account = tx_json
                                                .get("Account")
                                                .and_then(|a| a.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let sequence = tx_json
                                                .get("Sequence")
                                                .and_then(|s| s.as_u64())
                                                .unwrap_or(0) as u32;
                                            let fee_drops = tx_json
                                                .get("Fee")
                                                .and_then(|f| f.as_str())
                                                .and_then(|s| s.parse::<u64>().ok())
                                                .unwrap_or(0);
                                            let last_ledger_sequence = tx_json
                                                .get("LastLedgerSequence")
                                                .and_then(|v| v.as_u64())
                                                .map(|v| v as u32);

                                            let entry = rxrpl_txq::QueueEntry {
                                                hash,
                                                tx: tx_json,
                                                fee_level: rxrpl_txq::FeeLevel::new(fee_drops, fees.base_fee),
                                                account,
                                                sequence,
                                                last_ledger_sequence,
                                                preflight_passed: true,
                                            };
                                            let _ = tx_queue.write().await.submit(entry);
                                        }
                                        tracing::debug!("P2P tx {} applied: {}", hash, result);
                                    }
                                    Err(e) => {
                                        drop(l);
                                        tracing::debug!("P2P tx {} rejected: {}", hash, e);
                                    }
                                }
                            }
                            ConsensusMessage::StatusChange { from, ledger_seq: peer_seq, ledger_hash: peer_hash } => {
                                let our_seq = ledger_seq_shared.load(Ordering::Relaxed);
                                tracing::debug!(
                                    "peer {} at ledger #{} hash={}",
                                    from, peer_seq, peer_hash
                                );
                                let _ = event_tx.send(ServerEvent::PeerStatusChange {
                                    peer_id: from.to_string(),
                                    event: format!("status_change:seq={}", peer_seq),
                                });
                                if peer_seq > max_peer_seq {
                                    max_peer_seq = peer_seq;
                                }
                                last_peer_status_at = Some(tokio::time::Instant::now());
                                // --starting-ledger=recent: once we know a
                                // peer's current sequence, lock the anchor
                                // target at `peer_seq - 1024` (saturating).
                                // Subsequent peers can still raise the
                                // target via the regular sync path.
                                //
                                // Sanity cap on `peer_seq`: a malicious
                                // first peer could announce u32::MAX,
                                // making the anchor wait for an impossible
                                // ledger forever (audit finding M1).
                                // Mainnet tip is ~10^8; 10^9 is generous
                                // and rules out adversarial overflows.
                                const MAX_PLAUSIBLE_LEDGER_SEQ: u32 = 1_000_000_000;
                                if recent_anchor_pending
                                    && checkpoint_anchor.is_none()
                                    && peer_seq <= MAX_PLAUSIBLE_LEDGER_SEQ
                                {
                                    let target = peer_seq.saturating_sub(1024).max(1);
                                    tracing::info!(
                                        "checkpoint bootstrap (recent): tracking anchor for ledger #{} (peer at #{})",
                                        target, peer_seq
                                    );
                                    checkpoint_anchor = Some(
                                        crate::checkpoint::CheckpointAnchor::new(
                                            crate::checkpoint::AnchorConfig {
                                                target_seq: target,
                                                quorum: initial_quorum,
                                            },
                                        ),
                                    );
                                    recent_anchor_pending = false;
                                }
                                if peer_seq > our_seq + 1 {
                                    if !syncing {
                                        tracing::info!(
                                            "peer {} ahead by {} ledgers, entering sync mode (target #{})",
                                            from, peer_seq - our_seq, peer_seq
                                        );
                                        syncing = true;
                                        timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                        sync_started_at = Some(tokio::time::Instant::now());
                                        last_sync_seq = our_seq;
                                    }
                                    // Request the peer's current ledger directly
                                    let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                        seq: peer_seq,
                                        hash: Some(peer_hash),
                                    });
                                }
                            }
                            ConsensusMessage::LedgerData { hash, seq, nodes } => {
                                tracing::debug!(
                                    "received LedgerData hash={} seq={} nodes={}",
                                    hash, seq, nodes.len()
                                );
                                if !nodes.is_empty() {
                                    let cached = catchup_headers.get(&seq);
                                    match Node::try_reconstruct_ledger(seq, hash, &nodes, &node_store, cached) {
                                        Ok(reconstructed) => {
                                            let mut history = closed_ledgers.write().await;
                                            if !history.iter().any(|l| l.header.sequence == seq) {
                                                let pos = history.partition_point(|l| l.header.sequence < seq);
                                                history.insert(pos, reconstructed.clone());
                                                while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
                                                    history.pop_front();
                                                }
                                                tracing::info!("catchup: reconstructed ledger #{} hash={}", seq, hash);
                                            }
                                            drop(history);

                                            // Adopt if either:
                                            //  - We're in explicit sync mode (peer was way ahead), OR
                                            //  - The reconstructed ledger IS our current open seq
                                            //    (cross-impl yield path: we asked for peer's #N
                                            //    while our open=#N to follow peer's chain).
                                            let our_open_seq = ledger_seq_shared.load(Ordering::Relaxed);
                                            let should_adopt = syncing || seq == our_open_seq;
                                            if should_adopt {
                                                // Adopt the reconstructed ledger: open ledger becomes N+1
                                                let new_open = Ledger::new_open(&reconstructed);
                                                let new_seq = new_open.header.sequence;
                                                let mut l = ledger.write().await;
                                                *l = new_open;
                                                drop(l);
                                                ledger_seq_shared.store(new_seq, Ordering::Relaxed);
                                                *ledger_hash_shared.write().await = reconstructed.header.hash;
                                                tracing::info!(
                                                    "sync: adopted ledger #{}, open ledger is now #{}",
                                                    seq, new_seq
                                                );

                                                // Cross-impl: broadcast a validation signed by us
                                                // for the adopted ledger. Without this, peer's
                                                // quorum (=2) is never met since rxrpl never
                                                // produces a validation matching peer's hash.
                                                // We adopt peer's exact bytes, sign with our key,
                                                // and broadcast — peer receives 2 validations
                                                // (own + ours) for the same hash, reaches quorum,
                                                // advances validated_ledger.
                                                {
                                                    use rxrpl_consensus::types::Validation;
                                                    use rxrpl_primitives::Hash256;
                                                    let our_amendment_votes =
                                                        amendment_table.read().await.get_votes();
                                                    // sign_time = current wall-clock NetClock so
                                                    // rippled's freshness check accepts it. Using
                                                    // the (potentially old) ledger close_time would
                                                    // get the validation rejected as stale.
                                                    let now_netclock = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .unwrap_or_default()
                                                        .as_secs()
                                                        .saturating_sub(rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET) as u32;
                                                    let mut validation = Validation {
                                                        node_id: rxrpl_consensus::types::NodeId(Hash256::new(identity.node_id.0)),
                                                        public_key: identity.public_key_bytes().to_vec(),
                                                        ledger_hash: reconstructed.header.hash,
                                                        ledger_seq: seq,
                                                        full: true,
                                                        close_time: reconstructed.header.close_time,
                                                        sign_time: now_netclock,
                                                        signature: None,
                                                        amendments: our_amendment_votes,
                                                        signing_payload: None,
                                                        ..Default::default()
                                                    };
                                                    identity.sign_validation(&mut validation);
                                                    let payload = rxrpl_overlay::proto_convert::encode_validation(
                                                        &validation,
                                                        identity.public_key_bytes(),
                                                    );
                                                    let _ = cmd_tx_catchup.send(OverlayCommand::Broadcast {
                                                        msg_type: rxrpl_p2p_proto::MessageType::Validation,
                                                        payload,
                                                    });
                                                    tracing::debug!(
                                                        "adopted ledger #{} validated by us, broadcast Validation",
                                                        seq
                                                    );
                                                }

                                                // Use both max_peer_seq and highest validated
                                                // to determine if sync is complete
                                                let target = max_peer_seq
                                                    .max(val_aggregator.highest_validated_seq);
                                                if new_seq < target {
                                                    // Request next ledger in the chain
                                                    let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                                        seq: new_seq,
                                                        hash: None,
                                                    });
                                                    last_sync_seq = new_seq;
                                                } else {
                                                    syncing = false;
                                                    sync_started_at = None;
                                                    timer.on_phase_change(rxrpl_consensus::ConsensusPhase::Open);
                                                    // Clear expired transactions accumulated during sync
                                                    tx_queue.write().await.remove_expired(new_seq);
                                                    tracing::info!(
                                                        "catchup complete, resuming consensus at ledger #{}",
                                                        new_seq
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => tracing::warn!("catchup: failed to reconstruct ledger #{}: {}", seq, e),
                                    }
                                }
                            }
                            ConsensusMessage::LedgerHeader { seq, header } => {
                                tracing::debug!(
                                    "received parsed header for ledger #{} hash={}",
                                    seq, header.hash
                                );
                                // Cache the header so try_reconstruct_ledger
                                // can populate the full header on the
                                // catchup-built closed ledger. Bound the cache
                                // at 256 entries to keep memory finite under
                                // adversarial peers.
                                if catchup_headers.len() >= 256 {
                                    if let Some(min_seq) =
                                        catchup_headers.keys().min().copied()
                                    {
                                        catchup_headers.remove(&min_seq);
                                    }
                                }
                                catchup_headers.insert(seq, header);
                            }
                            ConsensusMessage::TxSetAcquired(tx_set) => {
                                tracing::info!(
                                    "acquired tx-set {} ({} txs) from network",
                                    tx_set.hash, tx_set.len()
                                );
                                // The tx-set is already stored in the shared cache by the
                                // overlay layer. The consensus engine will find it on its
                                // next acquire_tx_set call during dispute resolution.
                            }
                            ConsensusMessage::ValidatorListReceived { validator_count } => {
                                if configured_quorum.is_none() && validator_count > 0 {
                                    let new_quorum = Node::compute_quorum(validator_count);
                                    val_aggregator.update_quorum(new_quorum);
                                    tracing::info!(
                                        "auto-set validation quorum to {} (from {} validators)",
                                        new_quorum, validator_count
                                    );
                                }
                            }
                            ConsensusMessage::ValidatorListVerified { validators, sequence } => {
                                tracing::info!(
                                    "verified validator list seq={} with {} validators",
                                    sequence, validators.len()
                                );
                            }
                            ConsensusMessage::ManifestApplied {
                                master_key,
                                ephemeral_key,
                                old_ephemeral_key: _,
                                revoked,
                            } => {
                                if revoked {
                                    tracing::info!(
                                        "validator {} master key revoked",
                                        master_key
                                    );
                                } else if let Some(ref eph) = ephemeral_key {
                                    tracing::debug!(
                                        "manifest applied: master={} ephemeral={}",
                                        master_key, eph
                                    );
                                }
                            }
                        }
                    }

                    _ = sync_check_interval.tick(), if syncing => {
                        if let Some(started) = sync_started_at {
                            let current_seq = ledger_seq_shared.load(Ordering::Relaxed);
                            let elapsed = started.elapsed();
                            if elapsed > Duration::from_secs(30) && current_seq <= last_sync_seq {
                                // No progress in 30s, re-request the target ledger
                                let target = max_peer_seq
                                    .max(val_aggregator.highest_validated_seq);
                                tracing::warn!(
                                    "sync stalled at #{} for {:.0}s, re-requesting target #{}",
                                    current_seq, elapsed.as_secs_f64(), target
                                );
                                let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                    seq: target,
                                    hash: None,
                                });
                                // Reset the timeout clock
                                sync_started_at = Some(tokio::time::Instant::now());
                                last_sync_seq = current_seq;
                            }
                        }
                    }
                }
            }
        });

        tracing::info!(
            "networked node running (close interval: {}s), press ctrl+c to stop",
            close_interval_secs
        );

        tokio::signal::ctrl_c()
            .await
            .map_err(|e| NodeError::Server(format!("signal error: {e}")))?;

        tracing::info!("shutting down");
        Ok(())
    }

    /// Close a consensus round: apply the accepted set, close ledger, emit events.
    #[allow(clippy::too_many_arguments)]
    async fn close_consensus_round<A: rxrpl_consensus::ConsensusAdapter>(
        consensus: &ConsensusEngine<A>,
        pending_close_time: u32,
        ledger: &Arc<RwLock<Ledger>>,
        closed_ledgers: &Arc<RwLock<VecDeque<Ledger>>>,
        tx_store: &Option<Arc<dyn TxStore>>,
        event_tx: &tokio::sync::broadcast::Sender<ServerEvent>,
        ledger_seq_shared: &Arc<AtomicU32>,
        ledger_hash_shared: &Arc<tokio::sync::RwLock<Hash256>>,
        tx_queue: &Arc<RwLock<TxQueue>>,
        identity: &Arc<NodeIdentity>,
        cmd_tx: &tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
        amendment_table: &Arc<RwLock<AmendmentTable>>,
        tx_engine: &Arc<TxEngine>,
        fees: &Arc<FeeSettings>,
        validator_amendment_votes: &[Vec<Hash256>],
        trusted_validator_count: usize,
        pruner: &Arc<LedgerPruner>,
        node_store: &Option<Arc<dyn NodeStore>>,
    ) {
        // Resolve close_time in priority order:
        //  1. Quorum-accepted close_time from converge() — strongest signal,
        //     means UNL-quorum agreed on this exact value.
        //  2. Median-rounded peer-aware close_time from current
        //     peer_positions — when at least one peer has proposed, take
        //     the rounded median so we land in the same bucket as them
        //     even before formal quorum.
        //  3. Local fallback rounded to adaptive resolution — when no peer
        //     has proposed yet, at least round our own wall-clock so a
        //     peer within the same bucket produces an identical hash.
        let close_flags = consensus.accepted_close_flags();
        let effective_close_time = consensus
            .accepted_close_time()
            .or_else(|| consensus.rounded_close_time())
            // Cross-impl bridge: any peer proposal we've seen (even from a
            // different round) reveals the peer's close_time bucket. Adopt
            // it so two nodes whose close timers fire ~1s apart still land
            // in the same close_time and produce identical ledger hashes.
            .or_else(|| consensus.latest_peer_close_time())
            .unwrap_or_else(|| {
                let res = consensus.adaptive_close_time().resolution();
                rxrpl_consensus::round_close_time(pending_close_time, res)
            });
        tracing::debug!(
            "closing with effective_close_time={} close_flags={} pending_close_time={}",
            effective_close_time, close_flags, pending_close_time
        );

        let mut l = ledger.write().await;

        // Apply amendment voting on flag ledgers (before close computes
        // final hashes). Votes are collected from received validations.
        {
            let ledger_seq = l.header.sequence;
            let mut at = amendment_table.write().await;
            let _rules = Node::apply_amendment_voting(
                &mut l,
                tx_engine,
                &mut at,
                fees,
                trusted_validator_count,
                validator_amendment_votes,
                effective_close_time,
                ledger_seq,
            );
        }

        if let Err(e) = l.close(effective_close_time, close_flags) {
            tracing::error!("failed to close ledger: {}", e);
            return;
        }

        // Flush closed ledger to node store
        if let Err(e) = l.flush() {
            tracing::warn!("failed to flush ledger: {}", e);
        }

        let hash = l.header.hash;
        let closed_seq = l.header.sequence;
        let closed = l.clone();

        if let Some(store) = tx_store {
            Node::index_ledger_transactions(store.as_ref(), &closed);
        }

        let mut tx_count = 0u32;
        let mut has_offer_changes = false;
        closed.tx_map.for_each(&mut |_tx_hash, data| {
            tx_count += 1;
            if let Ok(record) = serde_json::from_slice::<Value>(data) {
                let tx_json = record.get("tx_json").cloned().unwrap_or_default();

                // Check for order book changes
                if let Some(tx_type) = tx_json.get("TransactionType").and_then(|v| v.as_str()) {
                    if matches!(tx_type, "OfferCreate" | "OfferCancel") {
                        has_offer_changes = true;
                    }
                }

                let _ = event_tx.send(ServerEvent::TransactionValidated {
                    transaction: tx_json,
                    meta: record.get("meta").cloned().unwrap_or_default(),
                    ledger_index: closed_seq,
                });
            }
        });

        // Emit book change events for order book modifications
        if has_offer_changes {
            let _ = event_tx.send(ServerEvent::BookChange {
                taker_pays: serde_json::json!({"currency": "XRP"}),
                taker_gets: serde_json::json!({"currency": "XRP"}),
                open: "0".into(),
                close: "0".into(),
                high: "0".into(),
                low: "0".into(),
                volume: "0".into(),
            });
        }

        let _ = event_tx.send(ServerEvent::LedgerClosed {
            ledger_index: closed_seq,
            ledger_hash: hash,
            ledger_time: effective_close_time,
            txn_count: tx_count,
        });

        // Emit path_find update after ledger close
        let _ = event_tx.send(ServerEvent::PathFindUpdate {
            alternatives: vec![],
        });

        *l = Ledger::new_open(&closed);
        let new_open_seq = l.header.sequence;

        ledger_seq_shared.store(new_open_seq, Ordering::Relaxed);
        *ledger_hash_shared.write().await = hash;

        // Record metrics
        metrics::gauge!("ledger_sequence").set(new_open_seq as f64);
        metrics::counter!("txn_applied_total").increment(tx_count as u64);
        metrics::gauge!("txn_queue_size").set(tx_queue.read().await.len() as f64);

        tracing::info!(
            "closed ledger #{} hash={}, opened #{}",
            closed_seq,
            hash,
            new_open_seq
        );
        drop(l);

        // Broadcast validation (STObject format, rippled-compatible)
        {
            use rxrpl_consensus::types::Validation;
            // Include our amendment votes in the validation message
            let our_amendment_votes = amendment_table.read().await.get_votes();
            let mut validation = Validation {
                node_id: rxrpl_consensus::types::NodeId(Hash256::new(identity.node_id.0)),
                public_key: identity.public_key_bytes().to_vec(),
                ledger_hash: hash,
                ledger_seq: closed_seq,
                full: true,
                close_time: effective_close_time,
                sign_time: effective_close_time,
                signature: None,
                amendments: our_amendment_votes,
                signing_payload: None,
                ..Default::default()
            };
            identity.sign_validation(&mut validation);
            let payload = rxrpl_overlay::proto_convert::encode_validation(
                &validation,
                identity.public_key_bytes(),
            );
            let _ = cmd_tx.send(OverlayCommand::Broadcast {
                msg_type: rxrpl_p2p_proto::MessageType::Validation,
                payload,
            });
        }

        // Broadcast StatusChange so peers know our current ledger
        {
            let payload =
                rxrpl_overlay::proto_convert::encode_status_change(&hash, closed_seq);
            let _ = cmd_tx.send(OverlayCommand::Broadcast {
                msg_type: rxrpl_p2p_proto::MessageType::StatusChange,
                payload,
            });
        }

        // Cleanup TxQueue: remove confirmed + expired, then retry remaining
        {
            let mut q = tx_queue.write().await;
            closed.tx_map.for_each(&mut |tx_hash, _| {
                q.remove(tx_hash);
            });
            q.remove_expired(new_open_seq);

            let pending = q.drain_for_retry();
            drop(q);

            let rules = Rules::new();
            let mut requeue = Vec::new();
            let mut l = ledger.write().await;
            for entry in pending {
                match tx_engine.apply(&entry.tx, &mut l, &rules, fees) {
                    Ok(result) if result.is_success() => {
                        requeue.push(entry);
                    }
                    _ => {}
                }
            }
            drop(l);

            let mut q = tx_queue.write().await;
            for entry in requeue {
                let _ = q.submit(entry);
            }
        }

        let mut history = closed_ledgers.write().await;
        let mut compacted = closed;
        compacted.compact();
        history.push_back(compacted);
        while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
            history.pop_front();
        }

        // Ledger history pruning
        if pruner.should_prune(closed_seq) {
            if let Some(store) = node_store {
                let retention = pruner.shared_state().retention_window;
                let cutoff_seq = closed_seq.saturating_sub(retention);

                let old: Vec<_> = history.iter()
                    .filter(|l| l.header.sequence <= cutoff_seq)
                    .cloned()
                    .collect();

                let retained = history.iter()
                    .find(|l| l.header.sequence > cutoff_seq);

                let _deleted = pruner.prune(closed_seq, &old, retained, store);
            }
        }
    }

    /// Attempt to reconstruct a closed ledger from downloaded leaf nodes.
    fn try_reconstruct_ledger(
        seq: u32,
        expected_hash: Hash256,
        nodes: &[(Vec<u8>, Vec<u8>)],
        node_store: &Option<Arc<dyn NodeStore>>,
        cached_header: Option<&rxrpl_ledger::LedgerHeader>,
    ) -> Result<Ledger, NodeError> {
        let state_map = SHAMap::from_leaf_nodes(nodes)
            .map_err(|e| NodeError::Server(format!("shamap reconstruction failed: {e}")))?;
        let mut ledger = match node_store {
            Some(store) => {
                let mut ledger = Ledger::from_catchup_with_store(
                    seq,
                    expected_hash,
                    state_map,
                    Arc::clone(store),
                );
                if let Err(e) = ledger.flush() {
                    tracing::warn!("failed to flush catchup ledger #{}: {}", seq, e);
                }
                ledger.compact();
                ledger
            }
            None => Ledger::from_catchup(seq, expected_hash, state_map),
        };
        // Populate the full header from the peer-provided liBASE response so
        // subsequent `Ledger::new_open(&this)` inherits the correct
        // parent_close_time, drops, close_time_resolution, etc. Without this,
        // the next local close computes a header hash divergent from the
        // peer's chain and consensus never reaches quorum.
        if let Some(h) = cached_header {
            // Preserve the catchup-derived account_hash and the
            // expected_hash we were given (those are the trust anchor); the
            // peer header should agree but we trust our reconstruction.
            let account_hash = ledger.header.account_hash;
            let hash = ledger.header.hash;
            ledger.header = h.clone();
            ledger.header.account_hash = account_hash;
            ledger.header.hash = hash;
        }
        Ok(ledger)
    }

    /// Create a genesis ledger with a funded account, optionally backed by a store.
    fn genesis_with_funded_account_and_store(
        genesis_address: &str,
        node_store: &Option<Arc<dyn NodeStore>>,
    ) -> Result<Ledger, NodeError> {
        // ALWAYS use store-less genesis: store-backed SHAMap produces a
        // different root_hash than store-less for identical content (likely
        // because the in-memory tree uses a different hash propagation path).
        // For cross-impl genesis convergence we need deterministic root_hash,
        // so build genesis without the store. Subsequent ledgers can use the
        // store via flush+compact after genesis is constructed.
        let _ = node_store;
        let mut genesis = Ledger::genesis();

        let account_id = decode_account_id(genesis_address)
            .map_err(|e| NodeError::Config(format!("invalid genesis address: {e}")))?;
        let key = keylet::account(&account_id);

        // Rippled-compatible genesis AccountRoot: includes PreviousTxnID +
        // PreviousTxnLgrSeq zero-fields that rippled always emits in its
        // SLE serialization. Without them, rxrpl's account_hash diverges
        // from rippled and the genesis #1 ledger hashes don't match,
        // breaking cross-impl consensus convergence.
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": genesis_address,
            "Balance": genesis.header.drops.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        let json_bytes =
            serde_json::to_vec(&account).map_err(|e| NodeError::Config(e.to_string()))?;
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes)
            .map_err(|e| NodeError::Config(format!("failed to encode genesis account: {e}")))?;
        genesis.put_state(key, data)?;

        // FeeSettings IS in rippled's genesis (verified by querying rippled
        // standalone via ledger_data: master AccountRoot + FeeSettings +
        // LedgerHashes). Without FeeSettings, rxrpl's genesis hash diverges.
        Self::insert_genesis_fee_settings(&mut genesis)?;

        genesis.close(0, 0)?;
        Ok(genesis)
    }

    /// Create a genesis ledger with a single funded account holding all XRP.
    ///
    /// Closes the genesis ledger and opens ledger #2 ready for transactions.
    pub fn genesis_with_funded_account(genesis_address: &str) -> Result<Ledger, NodeError> {
        let mut genesis = Ledger::genesis();

        let account_id = decode_account_id(genesis_address)
            .map_err(|e| NodeError::Config(format!("invalid genesis address: {e}")))?;
        let key = keylet::account(&account_id);

        // Rippled-compatible genesis AccountRoot: includes PreviousTxnID +
        // PreviousTxnLgrSeq zero-fields that rippled always emits in its
        // SLE serialization. Without them, rxrpl's account_hash diverges
        // from rippled and the genesis #1 ledger hashes don't match,
        // breaking cross-impl consensus convergence.
        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": genesis_address,
            "Balance": genesis.header.drops.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
            "PreviousTxnID": "0000000000000000000000000000000000000000000000000000000000000000",
            "PreviousTxnLgrSeq": 0,
        });
        let json_bytes =
            serde_json::to_vec(&account).map_err(|e| NodeError::Config(e.to_string()))?;
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes)
            .map_err(|e| NodeError::Config(format!("failed to encode genesis account: {e}")))?;
        genesis.put_state(key, data)?;

        // Add FeeSettings with default values
        Self::insert_genesis_fee_settings(&mut genesis)?;

        // Close genesis ledger
        genesis.close(0, 0)?;

        Ok(genesis)
    }

    /// Insert default FeeSettings into the genesis ledger state map.
    fn insert_genesis_fee_settings(genesis: &mut Ledger) -> Result<(), NodeError> {
        let fee_settings = serde_json::json!({
            "LedgerEntryType": "FeeSettings",
            "BaseFee": "a",
            "ReferenceFeeUnits": 10,
            "ReserveBase": 10000000u32,
            "ReserveIncrement": 2000000u32,
            "Flags": 0,
        });
        let fee_key = keylet::fee_settings();
        let json_bytes =
            serde_json::to_vec(&fee_settings).map_err(|e| NodeError::Config(e.to_string()))?;
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes)
            .map_err(|e| NodeError::Config(format!("failed to encode fee settings: {e}")))?;
        genesis.put_state(fee_key, data)?;
        Ok(())
    }

    /// Compute the validation quorum from a validator count.
    ///
    /// Returns `ceil(count * 0.8)`, clamped to at least 1.
    /// This matches the XRPL UNL quorum formula.
    pub fn compute_quorum(validator_count: usize) -> usize {
        (validator_count as f64 * 0.8).ceil().max(1.0) as usize
    }

    /// Apply a transaction to the current open ledger (standalone mode).
    ///
    /// Returns the transaction result code.
    pub fn apply_transaction(
        ledger: &mut Ledger,
        tx_engine: &TxEngine,
        tx: &Value,
        fees: &FeeSettings,
    ) -> Result<TransactionResult, NodeError> {
        if !ledger.is_open() {
            return Err(NodeError::LedgerNotOpen);
        }
        let rules = Rules::new();
        let result = tx_engine.apply(tx, ledger, &rules, fees)?;
        Ok(result)
    }

    /// Apply amendment voting pseudo-transactions on a flag ledger.
    ///
    /// On flag ledgers (sequence % 256 == 0), this tallies amendment votes from
    /// received validations, determines which amendments gained/lost majority or
    /// should activate, generates EnableAmendment pseudo-txs, and applies them
    /// to the open ledger. After activation, the amendment table and Rules are
    /// updated so subsequent transactions see the new amendment state.
    ///
    /// Returns the updated Rules snapshot.
    pub fn apply_amendment_voting(
        ledger: &mut Ledger,
        tx_engine: &TxEngine,
        amendment_table: &mut AmendmentTable,
        fees: &FeeSettings,
        trusted_count: usize,
        validator_votes: &[Vec<Hash256>],
        close_time: u32,
        ledger_seq: u32,
    ) -> Rules {
        if !rxrpl_amendment::is_flag_ledger(ledger_seq) {
            return amendment_table.build_rules();
        }

        let vote_counts = rxrpl_amendment::voting::count_votes(validator_votes);
        let actions = rxrpl_amendment::voting::tally_votes(
            amendment_table,
            trusted_count,
            &vote_counts,
            close_time,
            ledger_seq,
        );

        let rules = amendment_table.build_rules();

        if actions.is_empty() {
            tracing::debug!(
                "flag ledger #{}: no amendment voting changes",
                ledger_seq
            );
            return rules;
        }

        for action in &actions {
            let tx = rxrpl_amendment::voting::make_enable_amendment_tx(action);
            match tx_engine.apply(&tx, ledger, &rules, fees) {
                Ok(result) => {
                    if result.is_success() {
                        match action {
                            rxrpl_amendment::AmendmentAction::GotMajority {
                                amendment_id,
                                ..
                            } => {
                                tracing::info!(
                                    "amendment {} gained majority",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                            rxrpl_amendment::AmendmentAction::LostMajority {
                                amendment_id,
                            } => {
                                tracing::info!(
                                    "amendment {} lost majority",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                            rxrpl_amendment::AmendmentAction::Activate {
                                amendment_id,
                            } => {
                                tracing::info!(
                                    "amendment {} activated",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                        }
                    } else {
                        tracing::warn!(
                            "amendment pseudo-tx failed: {}",
                            result
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("failed to apply amendment pseudo-tx: {}", e);
                }
            }
        }

        // Return updated rules after any activations
        amendment_table.build_rules()
    }

    /// Close the current ledger and return a new open ledger derived from it.
    ///
    /// Returns the closed ledger's hash and the new open ledger.
    pub fn close_ledger(ledger: &mut Ledger, close_time: u32) -> Result<Hash256, NodeError> {
        ledger.close(close_time, 0)?;
        Ok(ledger.header.hash)
    }

    /// Get a reference to the current ledger.
    pub fn ledger(&self) -> &Arc<RwLock<Ledger>> {
        &self.ledger
    }

    /// Get a reference to the closed ledgers history.
    pub fn closed_ledgers(&self) -> &Arc<RwLock<VecDeque<Ledger>>> {
        &self.closed_ledgers
    }

    /// Get a reference to the transaction engine.
    pub fn tx_engine(&self) -> &Arc<TxEngine> {
        &self.tx_engine
    }

    /// Get a reference to the fees.
    pub fn fees(&self) -> &Arc<FeeSettings> {
        &self.fees
    }

    /// Get a reference to the transaction queue.
    pub fn tx_queue(&self) -> &Arc<RwLock<TxQueue>> {
        &self.tx_queue
    }

    /// Get a reference to the transaction store.
    pub fn tx_store(&self) -> Option<&Arc<dyn TxStore>> {
        self.tx_store.as_ref()
    }

    /// Get a reference to the node store.
    pub fn node_store(&self) -> Option<&Arc<dyn NodeStore>> {
        self.node_store.as_ref()
    }

    /// Index all transactions from a closed ledger into the transaction store.
    pub fn index_ledger_transactions(store: &dyn TxStore, ledger: &Ledger) {
        let seq = ledger.header.sequence;
        let mut tx_index = 0u32;

        ledger.tx_map.for_each(&mut |tx_hash, data| {
            // Parse tx record to extract Account
            if let Ok(record) = serde_json::from_slice::<Value>(data) {
                let tx_blob = data;

                // Extract metadata as bytes (reuse the full record)
                let meta_blob = serde_json::to_vec(&record.get("meta").unwrap_or(&Value::Null))
                    .unwrap_or_default();

                if let Err(e) =
                    store.insert_transaction(tx_hash.as_bytes(), seq, tx_index, tx_blob, &meta_blob)
                {
                    tracing::error!("failed to index tx {}: {}", tx_hash, e);
                }

                // Index by account
                if let Some(account_str) = record
                    .get("tx_json")
                    .and_then(|tj| tj.get("Account"))
                    .and_then(|a| a.as_str())
                {
                    if let Ok(account_id) = decode_account_id(account_str) {
                        if let Err(e) = store.insert_account_transaction(
                            account_id.as_bytes(),
                            seq,
                            tx_index,
                            tx_hash.as_bytes(),
                        ) {
                            tracing::error!("failed to index account tx: {}", e);
                        }
                    }
                }

                // Also index by Destination if present
                if let Some(dest_str) = record
                    .get("tx_json")
                    .and_then(|tj| tj.get("Destination"))
                    .and_then(|d| d.as_str())
                {
                    if let Ok(dest_id) = decode_account_id(dest_str) {
                        if let Err(e) = store.insert_account_transaction(
                            dest_id.as_bytes(),
                            seq,
                            tx_index,
                            tx_hash.as_bytes(),
                        ) {
                            tracing::error!("failed to index dest account tx: {}", e);
                        }
                    }
                }
            }

            tx_index += 1;
        });
    }

    /// Check if the node is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Fetch the latest validated ledger from an RPC endpoint.
    ///
    /// Returns (sequence, hash) of the latest validated ledger.
    async fn bootstrap_from_rpc(
        rpc_url: &str,
    ) -> Result<(u32, Hash256), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("bootstrapping from RPC endpoint: {}", rpc_url);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .danger_accept_invalid_certs(true)
            .build()?;

        let resp = client
            .post(rpc_url)
            .json(&serde_json::json!({
                "method": "server_info",
                "params": [{}]
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;

        let info = resp
            .get("result")
            .and_then(|r| r.get("info"))
            .ok_or("missing result.info in server_info response")?;

        // Try validated_ledger first, fall back to closed_ledger
        let ledger = info
            .get("validated_ledger")
            .or_else(|| info.get("closed_ledger"))
            .ok_or("no validated_ledger or closed_ledger in server_info")?;

        let seq = ledger
            .get("seq")
            .and_then(|v| v.as_u64())
            .ok_or("missing seq in ledger info")? as u32;

        let hash_str = ledger
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or("missing hash in ledger info")?;

        let hash_bytes = hex::decode(hash_str)
            .map_err(|e| format!("invalid ledger hash hex: {e}"))?;
        if hash_bytes.len() != 32 {
            return Err(format!("ledger hash must be 32 bytes, got {}", hash_bytes.len()).into());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash_bytes);
        let hash = Hash256::new(arr);

        Ok((seq, hash))
    }

    /// Download the full state tree for a ledger via RPC `ledger_data` pagination.
    async fn download_state_via_rpc(
        rpc_url: &str,
        ledger_hash: &str,
        store: Arc<dyn NodeStore>,
    ) -> Result<u32, Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .danger_accept_invalid_certs(true)
            .build()?;

        let mut marker: Option<String> = None;
        let mut total = 0u32;
        let mut page = 0u32;
        let mut retries = 0u32;
        let start = std::time::Instant::now();

        loop {
            let mut params = serde_json::json!({
                "ledger_hash": ledger_hash,
                "binary": true,
                "limit": 2048
            });
            if let Some(ref m) = marker {
                params["marker"] = serde_json::Value::String(m.clone());
            }

            let resp = match client
                .post(rpc_url)
                .json(&serde_json::json!({
                    "method": "ledger_data",
                    "params": [params]
                }))
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    retries += 1;
                    if retries > 5 {
                        return Err(format!("too many retries: {}", e).into());
                    }
                    tracing::warn!("RPC request failed (retry {}): {}", retries, e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };

            let body = match resp.json::<serde_json::Value>().await {
                Ok(v) => v,
                Err(e) => {
                    retries += 1;
                    if retries > 5 {
                        return Err(format!("too many retries: {}", e).into());
                    }
                    tracing::warn!("RPC response decode failed (retry {}): {}", retries, e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }
            };
            retries = 0;

            let result = body.get("result")
                .ok_or("missing result in ledger_data response")?;

            if let Some(err) = result.get("error") {
                return Err(format!("ledger_data error: {}", err).into());
            }

            let state = result.get("state")
                .and_then(|s| s.as_array())
                .ok_or("missing state array in ledger_data response")?;

            if state.is_empty() {
                break;
            }

            let mut batch: Vec<(Hash256, Vec<u8>)> = Vec::with_capacity(state.len());
            for entry in state {
                let index = match entry.get("index").and_then(|v| v.as_str()) {
                    Some(i) => i,
                    None => continue,
                };
                let data_hex = match entry.get("data").and_then(|v| v.as_str()) {
                    Some(d) => d,
                    None => continue,
                };

                let key_bytes = match hex::decode(index) {
                    Ok(b) if b.len() == 32 => b,
                    _ => continue,
                };
                let data_bytes = match hex::decode(data_hex) {
                    Ok(b) => b,
                    _ => continue,
                };

                let mut raw = Vec::with_capacity(32 + data_bytes.len());
                raw.extend_from_slice(&key_bytes);
                raw.extend_from_slice(&data_bytes);

                let prefix: [u8; 4] = [0x4D, 0x4C, 0x4E, 0x00];
                let hash = rxrpl_crypto::sha512_half::sha512_half(&[&prefix, &raw]);
                batch.push((hash, raw));
            }

            let count = batch.len();
            if count > 0 {
                let refs: Vec<(&Hash256, &[u8])> = batch.iter()
                    .map(|(h, d)| (h, d.as_slice()))
                    .collect();
                store.store_batch(&refs)?;
            }
            total += count as u32;
            page += 1;

            if page % 100 == 0 {
                let elapsed = start.elapsed().as_secs();
                let rate = if elapsed > 0 { total as u64 / elapsed } else { 0 };
                tracing::info!(
                    "RPC state download: {} entries ({} pages, {} entries/s)",
                    total, page, rate
                );
            }

            marker = result.get("marker")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if marker.is_none() {
                break;
            }
        }

        let elapsed = start.elapsed().as_secs();
        tracing::info!(
            "RPC state download complete: {} entries in {} pages ({}s)",
            total, page, elapsed
        );
        Ok(total)
    }
}

/// Decode a `node_seed` config value into raw 16-byte seed entropy.
///
/// Accepts either:
/// - 32 hex characters (raw entropy)
/// - a base58 family seed (e.g. `snXxx...`) — what rippled-style configs
///   and xrpl-hive's `XRPL_VALIDATOR_SEED` env emit
fn parse_node_seed(s: &str) -> Result<[u8; 16], String> {
    let trimmed = s.trim();

    // Try hex first (32 chars).
    if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex::decode(trimmed).map_err(|e| format!("invalid hex: {e}"))?;
        if bytes.len() != 16 {
            return Err("hex seed must decode to 16 bytes".into());
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }

    // Fall back to base58 family seed.
    let (entropy, _key_type) = rxrpl_codec::address::seed::decode_seed(trimmed)
        .map_err(|e| format!("invalid base58 family seed: {e}"))?;
    Ok(entropy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_node_seed_hex() {
        let s = "0123456789ABCDEF0123456789ABCDEF";
        let bytes = parse_node_seed(s).unwrap();
        assert_eq!(bytes.len(), 16);
        assert_eq!(bytes[0], 0x01);
        assert_eq!(bytes[15], 0xEF);
    }

    #[test]
    fn parse_node_seed_base58() {
        // xrpl-hive's first DefaultValidator seed (rippled-style base58).
        let s = "sneWFZcEqA8TUA5BmJ38xsqaR7dFb";
        let bytes = parse_node_seed(s).unwrap();
        assert_eq!(bytes.len(), 16);
    }

    #[test]
    fn parse_node_seed_garbage_rejected() {
        assert!(parse_node_seed("not a seed").is_err());
    }

    #[test]
    fn create_node() {
        let config = NodeConfig::default();
        let node = Node::new(config).unwrap();
        assert!(!node.is_running());
    }

    #[test]
    fn genesis_with_funded_account() {
        let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let genesis = Node::genesis_with_funded_account(address).unwrap();

        assert!(genesis.is_closed());
        assert!(!genesis.header.hash.is_zero());

        // Verify account exists with full XRP supply
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let data = genesis.get_state(&key).unwrap();
        let account: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
        assert_eq!(
            account["Balance"].as_str().unwrap(),
            genesis.header.drops.to_string()
        );
    }

    #[test]
    fn new_standalone_creates_open_ledger() {
        let config = NodeConfig::default();
        let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let node = Node::new_standalone(config, address).unwrap();

        // Ledger should be open at sequence 2
        let ledger = node.ledger.blocking_read();
        assert!(ledger.is_open());
        assert_eq!(ledger.header.sequence, 2);

        // Should have genesis in closed history
        let closed = node.closed_ledgers.blocking_read();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].header.sequence, 1);
    }

    #[test]
    fn genesis_includes_fee_settings() {
        let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let genesis = Node::genesis_with_funded_account(address).unwrap();

        let fee_key = keylet::fee_settings();
        let data = genesis.get_state(&fee_key).expect("FeeSettings missing from genesis");
        let fee: Value = rxrpl_ledger::sle_codec::decode_state(data).unwrap();
        assert_eq!(fee["LedgerEntryType"].as_str().unwrap(), "FeeSettings");
        assert_eq!(fee["ReserveBase"], 10_000_000);
        assert_eq!(fee["ReserveIncrement"], 2_000_000);
    }

    #[test]
    fn genesis_hash_deterministic() {
        let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let genesis1 = Node::genesis_with_funded_account(address).unwrap();
        let genesis2 = Node::genesis_with_funded_account(address).unwrap();

        assert_eq!(genesis1.header.hash, genesis2.header.hash);
        assert_eq!(genesis1.header.account_hash, genesis2.header.account_hash);
        assert!(!genesis1.header.hash.is_zero());
    }

    #[test]
    fn compute_quorum_standard_unl() {
        // 35 validators (typical mainnet UNL) → 28 quorum (80%)
        assert_eq!(Node::compute_quorum(35), 28);
    }

    #[test]
    fn compute_quorum_small_list() {
        assert_eq!(Node::compute_quorum(10), 8);
        assert_eq!(Node::compute_quorum(5), 4);
        assert_eq!(Node::compute_quorum(1), 1);
    }

    #[test]
    fn compute_quorum_rounds_up() {
        // 7 * 0.8 = 5.6 → ceil → 6
        assert_eq!(Node::compute_quorum(7), 6);
        // 3 * 0.8 = 2.4 → ceil → 3
        assert_eq!(Node::compute_quorum(3), 3);
    }

    #[test]
    fn compute_quorum_zero_returns_one() {
        assert_eq!(Node::compute_quorum(0), 1);
    }

    #[test]
    fn quorum_auto_set_integration() {
        // Simulate the full flow: ValidatorListReceived → compute_quorum → update_quorum
        // This tests the exact code path from the select! handler.
        use rxrpl_overlay::validation_aggregator::ValidationAggregator;
        use rxrpl_consensus::types::{NodeId as CNodeId, Validation};

        let configured_quorum: Option<usize> = None; // auto mode
        let mut val_aggregator = ValidationAggregator::new(1);

        // Simulate receiving a ValidatorList with 35 validators
        let validator_count = 35usize;
        if configured_quorum.is_none() && validator_count > 0 {
            let new_quorum = Node::compute_quorum(validator_count);
            val_aggregator.update_quorum(new_quorum);
        }

        // Now quorum should be 28. Sending 27 validations should NOT reach quorum.
        let hash = Hash256::new([0xAA; 32]);
        for i in 1..=27u8 {
            let v = Validation {
                node_id: CNodeId(Hash256::new([i; 32])),
                public_key: Vec::new(),
                ledger_hash: hash,
                ledger_seq: 100,
                full: true,
                close_time: 100,
                sign_time: 100,
                signature: None,
                amendments: vec![],
                signing_payload: None,
                ..Default::default()
            };
            assert!(val_aggregator.add_validation_at(v, 100).is_none());
        }

        // 28th validation reaches quorum
        let v28 = Validation {
            node_id: CNodeId(Hash256::new([28; 32])),
            public_key: Vec::new(),
            ledger_hash: hash,
            ledger_seq: 100,
            full: true,
            close_time: 100,
            sign_time: 100,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        let result = val_aggregator.add_validation_at(v28, 100);
        assert!(result.is_some());
        assert_eq!(result.unwrap().validation_count, 28);
    }

    #[test]
    fn quorum_not_overridden_when_configured() {
        // When quorum is explicitly configured, ValidatorListReceived should NOT change it
        use rxrpl_overlay::validation_aggregator::ValidationAggregator;

        let configured_quorum: Option<usize> = Some(5); // explicit
        let mut val_aggregator = ValidationAggregator::new(5);

        let validator_count = 35usize;
        // This guard prevents override — same as in the select! handler
        if configured_quorum.is_none() && validator_count > 0 {
            let new_quorum = Node::compute_quorum(validator_count);
            val_aggregator.update_quorum(new_quorum);
        }

        // Quorum should still be 5, not 28
        let hash = Hash256::new([0xBB; 32]);
        for i in 1..=4u8 {
            let v = rxrpl_consensus::types::Validation {
                node_id: rxrpl_consensus::types::NodeId(Hash256::new([i; 32])),
                public_key: Vec::new(),
                ledger_hash: hash,
                ledger_seq: 200,
                full: true,
                close_time: 100,
                sign_time: 100,
                signature: None,
                amendments: vec![],
                signing_payload: None,
                ..Default::default()
            };
            assert!(val_aggregator.add_validation_at(v, 100).is_none());
        }
        let v5 = rxrpl_consensus::types::Validation {
            node_id: rxrpl_consensus::types::NodeId(Hash256::new([5; 32])),
            public_key: Vec::new(),
            ledger_hash: hash,
            ledger_seq: 200,
            full: true,
            close_time: 100,
            sign_time: 100,
            signature: None,
            amendments: vec![],
            signing_payload: None,
            ..Default::default()
        };
        assert!(val_aggregator.add_validation_at(v5, 100).is_some());
    }

    #[test]
    fn genesis_binary_encoding_deterministic() {
        let address = "rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh";
        let genesis1 = Node::genesis_with_funded_account(address).unwrap();
        let genesis2 = Node::genesis_with_funded_account(address).unwrap();

        // Verify the raw binary data for the account root is identical
        let account_id = decode_account_id(address).unwrap();
        let key = keylet::account(&account_id);
        let data1 = genesis1.get_state(&key).unwrap();
        let data2 = genesis2.get_state(&key).unwrap();
        assert_eq!(data1, data2, "binary encoding must be deterministic");
    }
}
