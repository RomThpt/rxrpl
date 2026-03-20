use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rxrpl_amendment::{AmendmentTable, FeatureRegistry, Rules};
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_config::NodeConfig;
use rxrpl_consensus::{ConsensusEngine, ConsensusParams, NodeId, TrustedValidatorList, TxSet};
use rxrpl_ledger::Ledger;
use rxrpl_overlay::{
    ConsensusMessage, LedgerProvider, NetworkConsensusAdapter, NodeIdentity, OverlayCommand,
    PeerManager, PeerManagerConfig,
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
    config: NodeConfig,
    ledger: Arc<RwLock<Ledger>>,
    closed_ledgers: Arc<RwLock<VecDeque<Ledger>>>,
    tx_engine: Arc<TxEngine>,
    tx_queue: Arc<RwLock<TxQueue>>,
    amendment_table: Arc<RwLock<AmendmentTable>>,
    fees: Arc<FeeSettings>,
    tx_store: Option<Arc<dyn TxStore>>,
    node_store: Option<Arc<dyn NodeStore>>,
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
        rxrpl_tx_engine::handlers::register_batch(&mut tx_registry);
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
        rxrpl_tx_engine::handlers::register_batch(&mut tx_registry);
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
        let ctx = ServerContext::with_node_state(
            self.config.server.clone(),
            Arc::clone(&self.ledger),
            Arc::clone(&self.closed_ledgers),
            Arc::clone(&self.tx_engine),
            Arc::clone(&self.fees),
            self.tx_store.as_ref().map(Arc::clone),
            Some(Arc::clone(&self.tx_queue)),
            None, // no relay in standalone mode
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
        let event_tx = event_tx.clone();
        let interval_duration = Duration::from_secs(close_interval_secs);

        tokio::spawn(async move {
            let adapter = NodeConsensusAdapter::new();
            let node_id = NodeId(Hash256::new([0x01; 32]));
            let mut consensus = ConsensusEngine::new(adapter, node_id, ConsensusParams::default());

            let mut interval = tokio::time::interval(interval_duration);
            // Skip the first immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                let close_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32;

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

                // Cleanup TxQueue: remove confirmed + expired
                {
                    let mut q = tx_queue.write().await;
                    closed.tx_map.for_each(&mut |tx_hash, _| {
                        q.remove(tx_hash);
                    });
                    q.remove_expired(new_open_seq);
                }

                // Store in history (compact for memory efficiency)
                let mut history = closed_ledgers.write().await;
                let mut compacted = closed;
                compacted.compact();
                history.push_back(compacted);
                while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
                    history.pop_front();
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
    pub async fn run_networked(&self, close_interval_secs: u64) -> Result<(), NodeError> {
        // 1. Generate/load node identity
        let identity = if let Some(ref seed_hex) = self.config.peer.node_seed {
            let bytes = hex::decode(seed_hex)
                .map_err(|e| NodeError::Config(format!("invalid node_seed hex: {e}")))?;
            if bytes.len() != 16 {
                return Err(NodeError::Config(
                    "node_seed must be 16 bytes (32 hex chars)".into(),
                ));
            }
            let mut seed_bytes = [0u8; 16];
            seed_bytes.copy_from_slice(&bytes);
            NodeIdentity::from_seed(&rxrpl_crypto::Seed::from_bytes(seed_bytes))
        } else {
            NodeIdentity::generate()
        };
        let identity = Arc::new(identity);
        tracing::info!("node identity: {}", identity.node_id);
        tracing::info!("node public key: {}", hex::encode(identity.public_key_bytes()));

        // 2. Shared ledger state for P2P
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

        // 6. Spawn relay bridge: RPC submit -> P2P broadcast
        tokio::spawn(async move {
            while let Some((tx_hash, tx_bytes)) = relay_rx.recv().await {
                let payload = rxrpl_overlay::proto_convert::encode_transaction(&tx_hash, &tx_bytes);
                let _ = cmd_tx_relay.send(OverlayCommand::Broadcast {
                    msg_type: rxrpl_p2p_proto::MessageType::Transaction,
                    payload,
                });
            }
        });

        // 7. Start RPC server
        let ctx = ServerContext::with_node_state(
            self.config.server.clone(),
            Arc::clone(&self.ledger),
            Arc::clone(&self.closed_ledgers),
            Arc::clone(&self.tx_engine),
            Arc::clone(&self.fees),
            self.tx_store.as_ref().map(Arc::clone),
            Some(Arc::clone(&self.tx_queue)),
            Some(relay_tx),
        );
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
        let unl = {
            let mut trusted = HashSet::new();
            for pk_hex in &self.config.validators.trusted {
                if let Ok(bytes) = hex::decode(pk_hex) {
                    trusted.insert(NodeId::from_public_key(&bytes));
                }
            }
            if !trusted.is_empty() {
                tracing::info!("UNL configured with {} trusted validators", trusted.len());
            }
            TrustedValidatorList::new(trusted)
        };

        // 8. Consensus loop with multi-round convergence
        let ledger = Arc::clone(&self.ledger);
        let closed_ledgers = Arc::clone(&self.closed_ledgers);
        let tx_engine = Arc::clone(&self.tx_engine);
        let fees = Arc::clone(&self.fees);
        let tx_queue = Arc::clone(&self.tx_queue);
        let node_store = self.node_store.clone();
        let tx_store = self.tx_store.as_ref().map(Arc::clone);
        let interval_duration = Duration::from_secs(close_interval_secs);
        let ledger_seq_shared = Arc::clone(&ledger_seq);
        let ledger_hash_shared = Arc::clone(&ledger_hash);

        tokio::spawn(async move {
            let node_id = NodeId(identity.node_id);
            let mut consensus =
                ConsensusEngine::new_with_unl(adapter, node_id, identity.public_key_bytes().to_vec(), ConsensusParams::default(), unl);

            let mut close_interval = tokio::time::interval(interval_duration);
            let converge_duration = Duration::from_millis(1250);
            let mut converge_interval = tokio::time::interval(converge_duration);
            let mut establishing = false;
            let mut syncing = false;
            let mut max_peer_seq: u32 = 0;
            let mut pending_close_time = 0u32;

            close_interval.tick().await; // skip first immediate tick
            converge_interval.tick().await;

            loop {
                tokio::select! {
                    _ = close_interval.tick(), if !establishing && !syncing => {
                        let close_time = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as u32;

                        let l = ledger.read().await;
                        let prev_hash = l.header.parent_hash;
                        let seq = l.header.sequence;

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

                        pending_close_time = close_time;

                        let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                            phase: "open".into(),
                        });

                        // Try immediate convergence (solo mode or instant agreement)
                        if consensus.converge() {
                            let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                phase: "accepted".into(),
                            });
                            Self::close_consensus_round(
                                &consensus, pending_close_time, &ledger,
                                &closed_ledgers, &tx_store, &event_tx,
                                &ledger_seq_shared, &ledger_hash_shared,
                                &tx_queue, &identity, &cmd_tx_catchup,
                            ).await;
                            close_interval.reset();
                        } else {
                            let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                phase: "establish".into(),
                            });
                            establishing = true;
                            converge_interval.reset();
                        }
                    }

                    _ = converge_interval.tick(), if establishing => {
                        if consensus.converge() {
                            let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                phase: "accepted".into(),
                            });
                            Self::close_consensus_round(
                                &consensus, pending_close_time, &ledger,
                                &closed_ledgers, &tx_store, &event_tx,
                                &ledger_seq_shared, &ledger_hash_shared,
                                &tx_queue, &identity, &cmd_tx_catchup,
                            ).await;
                            establishing = false;
                            close_interval.reset();
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
                            }
                            ConsensusMessage::Validation(validation) => {
                                tracing::debug!(
                                    "validation from {:?} for ledger #{} hash={}",
                                    validation.node_id, validation.ledger_seq, validation.ledger_hash
                                );
                                let _ = event_tx.send(ServerEvent::ValidationReceived {
                                    validator: validation.node_id.0.to_string(),
                                    ledger_hash: validation.ledger_hash.to_string(),
                                    ledger_seq: validation.ledger_seq,
                                    full: validation.full,
                                });
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
                                if peer_seq > our_seq + 1 {
                                    if !syncing {
                                        tracing::info!(
                                            "peer {} ahead by {} ledgers, entering sync mode",
                                            from, peer_seq - our_seq
                                        );
                                        syncing = true;
                                    }
                                    let next_seq = our_seq + 1;
                                    let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                        seq: next_seq,
                                        hash: None,
                                    });
                                }
                            }
                            ConsensusMessage::LedgerData { hash, seq, nodes } => {
                                tracing::debug!(
                                    "received LedgerData hash={} seq={} nodes={}",
                                    hash, seq, nodes.len()
                                );
                                if !nodes.is_empty() {
                                    match Node::try_reconstruct_ledger(seq, hash, &nodes, &node_store) {
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

                                            if syncing {
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

                                                if new_seq < max_peer_seq {
                                                    // Request next ledger in the chain
                                                    let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                                        seq: new_seq,
                                                        hash: None,
                                                    });
                                                } else {
                                                    syncing = false;
                                                    close_interval.reset();
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
    ) {
        let effective_close_time = consensus
            .accepted_close_time()
            .unwrap_or(pending_close_time);
        let close_flags = consensus.accepted_close_flags();
        tracing::debug!(
            "closing with effective_close_time={} close_flags={} pending_close_time={}",
            effective_close_time, close_flags, pending_close_time
        );

        let mut l = ledger.write().await;
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
            let mut validation = Validation {
                node_id: rxrpl_consensus::types::NodeId(Hash256::new(identity.node_id.0)),
                ledger_hash: hash,
                ledger_seq: closed_seq,
                full: true,
                close_time: effective_close_time,
                sign_time: effective_close_time,
                signature: None,
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

        // Cleanup TxQueue: remove confirmed + expired
        {
            let mut q = tx_queue.write().await;
            closed.tx_map.for_each(&mut |tx_hash, _| {
                q.remove(tx_hash);
            });
            q.remove_expired(new_open_seq);
        }

        let mut history = closed_ledgers.write().await;
        let mut compacted = closed;
        compacted.compact();
        history.push_back(compacted);
        while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
            history.pop_front();
        }
    }

    /// Attempt to reconstruct a closed ledger from downloaded leaf nodes.
    fn try_reconstruct_ledger(
        seq: u32,
        expected_hash: Hash256,
        nodes: &[(Vec<u8>, Vec<u8>)],
        node_store: &Option<Arc<dyn NodeStore>>,
    ) -> Result<Ledger, NodeError> {
        let state_map = SHAMap::from_leaf_nodes(nodes)
            .map_err(|e| NodeError::Server(format!("shamap reconstruction failed: {e}")))?;
        match node_store {
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
                Ok(ledger)
            }
            None => Ok(Ledger::from_catchup(seq, expected_hash, state_map)),
        }
    }

    /// Create a genesis ledger with a funded account, optionally backed by a store.
    fn genesis_with_funded_account_and_store(
        genesis_address: &str,
        node_store: &Option<Arc<dyn NodeStore>>,
    ) -> Result<Ledger, NodeError> {
        let mut genesis = match node_store {
            Some(store) => Ledger::genesis_with_store(Arc::clone(store)),
            None => Ledger::genesis(),
        };

        let account_id = decode_account_id(genesis_address)
            .map_err(|e| NodeError::Config(format!("invalid genesis address: {e}")))?;
        let key = keylet::account(&account_id);

        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": genesis_address,
            "Balance": genesis.header.drops.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
        });
        let json_bytes =
            serde_json::to_vec(&account).map_err(|e| NodeError::Config(e.to_string()))?;
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes)
            .map_err(|e| NodeError::Config(format!("failed to encode genesis account: {e}")))?;
        genesis.put_state(key, data)?;

        // Add FeeSettings with default values
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

        let account = serde_json::json!({
            "LedgerEntryType": "AccountRoot",
            "Account": genesis_address,
            "Balance": genesis.header.drops.to_string(),
            "Sequence": 1,
            "OwnerCount": 0,
            "Flags": 0,
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
