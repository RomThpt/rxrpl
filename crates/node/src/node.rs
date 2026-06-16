use std::collections::{HashSet, VecDeque};
#[cfg(feature = "grpc")]
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rxrpl_amendment::{AmendmentTable, FeatureRegistry, Rules};
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_config::{NodeConfig, load_seed_file};
use rxrpl_consensus::{
    ConsensusEngine, ConsensusParams, ConsensusTimer, NodeId, TimerAction, TrustedValidatorList,
    TxSet,
};
use rxrpl_crypto::Seed;
use rxrpl_ledger::Ledger;
#[cfg(feature = "rocksdb")]
use rxrpl_nodestore::PersistentNodeDatabase;
use rxrpl_nodestore::{CachedNodeStore, MemoryNodeDatabase};
use rxrpl_overlay::{
    ConsensusMessage, LedgerProvider, NetworkConsensusAdapter, NodeIdentity, OverlayCommand,
    PeerManager, PeerManagerConfig, VlFetcher, new_trusted_keys,
};
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{TransactionResult, keylet};
use rxrpl_rpc_server::{ServerContext, ServerEvent};
use rxrpl_shamap::{NodeStore, SHAMap};
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
    /// Optional validator signing seed loaded from `validators.seed_file`.
    /// Held only when `validators.enabled` is true; otherwise dropped after
    /// emitting a warning so unused secret material is zeroized promptly.
    validation_seed: Option<Seed>,
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

    /// Reference to the loaded validator signing seed, if any.
    #[allow(dead_code)]
    pub(crate) fn validation_seed(&self) -> Option<&Seed> {
        self.validation_seed.as_ref()
    }

    /// Load the validator signing seed if configured, enforcing strict
    /// permissions. Returns `Ok(None)` when no `seed_file` is set; emits a
    /// warning (and drops the seed) when a seed file is provided but
    /// validation is not enabled.
    fn load_validation_seed(config: &NodeConfig) -> Result<Option<Seed>, NodeError> {
        let Some(path) = config.validators.seed_file.as_deref() else {
            return Ok(None);
        };
        let seed = load_seed_file(path).map_err(|e| NodeError::SeedFile(e.to_string()))?;
        if !config.validators.enabled {
            tracing::warn!(
                path = %path.display(),
                "validator seed file configured but [validators].enabled is false; \
                 seed will not be used"
            );
            // Drop seed to zeroize unused secret material.
            drop(seed);
            return Ok(None);
        }
        tracing::info!(path = %path.display(), "validator signing seed loaded");
        Ok(Some(seed))
    }

    /// Create a new node from configuration.
    pub fn new(config: NodeConfig) -> Result<Self, NodeError> {
        let validation_seed = Self::load_validation_seed(&config)?;

        // Initialize node store
        let node_store = Self::create_node_store(&config)?;

        // Initialize amendment registry
        let registry = FeatureRegistry::with_known_amendments();
        let mut amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4); // ~14 days at 4s/ledger
        config
            .amendments
            .apply(&registry, &mut amendment_table)
            .map_err(|e| NodeError::Config(format!("amendments: {e}")))?;

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

        // Initialize genesis ledger. The networked path (`run_networked`)
        // uses this constructor, so the genesis MUST carry the same
        // FeeSettings + Amendments SLEs as rippled-2.6.2 or every close
        // after #1 diverges: rippled's `LedgerHashes` skip-list at seq=2
        // includes parent_hash=rippled-genesis, ours included
        // parent_hash=bare-genesis, the SLE bytes differed → account_hash
        // diverged → wrong_prev_ledger feedback loop. Using the store-less
        // ledger keeps SHAMap root-hash deterministic across builds (the
        // store-backed variant propagates hashes via a different path and
        // produces a different root for identical content).
        let _ = &node_store; // SHAMap store attached lazily during close().
        // Networked genesis MUST match what peers will reconstruct from
        // their own genesis bootstrap. xrpl-confluence sets rippled
        // `genesis_amendments_disabled = true`, so kurtosis rippled
        // genesis contains ONLY the canonical XRPL master AccountRoot
        // (`rHb9CJAW…`, 100B drops) — no FeeSettings SLE and no
        // Amendments SLE. Adding either makes our account_hash diverge
        // from rippled, every close after #1 produces a mismatching
        // hash, and rxrpl falls into the catchup feedback loop seen on
        // 2026-05-12.
        let ledger = Self::genesis_with_master_account_only("rHb9CJAWyB4rj91VRWn96DkukG4bwdtyTh")?;

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
            validation_seed,
            running: false,
        })
    }

    /// Create a standalone node with a funded genesis account.
    ///
    /// Creates genesis ledger, funds the account, closes genesis,
    /// and opens ledger #2 ready for transactions.
    pub fn new_standalone(config: NodeConfig, genesis_address: &str) -> Result<Self, NodeError> {
        let validation_seed = Self::load_validation_seed(&config)?;
        let node_store = Self::create_node_store(&config)?;

        let registry = FeatureRegistry::with_known_amendments();
        let mut amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4);
        config
            .amendments
            .apply(&registry, &mut amendment_table)
            .map_err(|e| NodeError::Config(format!("amendments: {e}")))?;

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

        // Select the genesis layout to match the rippled peers we connect
        // to: stock rippled (master AccountRoot + FeeSettings + Amendments)
        // vs `genesis_amendments_disabled = true` (master AccountRoot only).
        let mut closed_genesis = if config.network.genesis_amendments_disabled {
            Self::genesis_with_master_account_only(genesis_address)?
        } else {
            Self::genesis_with_funded_account_and_store(genesis_address, &node_store)?
        };

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
            validation_seed,
            running: false,
        })
    }

    /// Start the node (RPC server and peer networking).
    pub async fn start(&mut self) -> Result<(), NodeError> {
        if self.running {
            return Err(NodeError::AlreadyRunning);
        }

        let ctx = ServerContext::new(self.config.server.clone());

        rxrpl_rpc_server::serve(ctx, self.config.server.bind, self.config.server.ws_bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        self.running = true;

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

        rxrpl_rpc_server::serve(ctx, self.config.server.bind, self.config.server.ws_bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

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
                    .saturating_sub(rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET)
                    as u32;

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
                let parent_close_time = l.header.parent_close_time;

                // Build the consensus candidate set (with canonical blobs)
                // from the open ledger.
                let tx_set = Node::collect_consensus_tx_set(&l);
                drop(l);

                // Run consensus (solo = immediate accept). Pass the parent's
                // close_time so eff_close_time clamps to parent+1 (rippled's
                // monotonicity guarantee). The legacy start_round defaults
                // prior to 0, which silently disables the clamp and forks
                // the ledger hash 1 bucket from rippled every round.
                consensus.start_round_with_prior(prev_hash, ledger_seq, parent_close_time);
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

                // Apply negative-UNL pseudo-transactions on flag ledgers
                // (no-op otherwise). Mirrors apply_amendment_voting:
                // pseudo-txs land before the ledger's final hash is
                // computed by close().
                let _nunl_results = Node::apply_negative_unl(
                    &mut consensus,
                    &mut l,
                    &tx_engine_close,
                    &fees_close,
                    ledger_seq,
                );
                consensus.on_ledger_close_for_tracker();

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
                        if let Some(tx_type) =
                            tx_json.get("TransactionType").and_then(|v| v.as_str())
                        {
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
                        let old: Vec<_> = history
                            .iter()
                            .filter(|l| l.header.sequence <= cutoff_seq)
                            .cloned()
                            .collect();

                        // The retained ledger is the first one after the cutoff
                        let retained = history.iter().find(|l| l.header.sequence > cutoff_seq);

                        let _deleted = pruner.prune(seq, &old, retained, store);
                    }
                }
            }
        });

        tracing::info!(
            "standalone node running (close interval: {}s), waiting for SIGINT/SIGTERM",
            close_interval_secs
        );

        let signal = crate::shutdown::wait_for_shutdown()
            .await
            .map_err(|e| NodeError::Server(format!("signal error: {e}")))?;

        tracing::info!(signal = %signal, "shutting down gracefully");
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
        tracing::info!(
            "node public key: {}",
            hex::encode(identity.public_key_bytes())
        );

        // 1b. Optional validator signing identity (master + ephemeral). When
        // configured, validations + proposals are signed by the ephemeral key
        // (with master key publishing the manifest). When absent, the
        // legacy single-key path uses `identity` for signing too.
        let validator_id: Option<Arc<rxrpl_overlay::identity::ValidatorIdentity>> =
            build_validator_identity(&self.config.validator_identity)?.map(Arc::new);
        if let Some(ref vid) = validator_id {
            tracing::info!(
                "validator signing identity loaded: master={}, signing={}",
                hex::encode(vid.master_pubkey().as_bytes()),
                hex::encode(vid.signing_pubkey().as_bytes()),
            );
        }

        // 1c. Build our own signed manifest from the validator identity, so
        // peers can bind our `signing_pubkey` to our `master_pubkey` when
        // they receive our validations. Without this, rippled classifies
        // our validations as `untrusted`. The ManifestStore registration
        // happens after PeerManager construction below (`peer_mgr.set_local_manifest`).
        //
        // B4: read any previously-persisted manifest. If we already
        // published sequence N on a prior run, the new sequence must be
        // strictly greater (rotation contract); otherwise peers reject
        // our manifest as stale and our validations remain untrusted.
        let local_manifest = match validator_id.as_deref() {
            Some(vid) => {
                let data_dir = &self.config.database.path;
                let persisted = match crate::local_manifest_store::load(data_dir) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "failed to load persisted local manifest at {}: {} — using config sequence",
                            data_dir.display(),
                            e,
                        );
                        None
                    }
                };
                let cfg_seq = self.config.validator_identity.sequence;
                let seq = match persisted.as_ref() {
                    Some(p) => p.sequence.saturating_add(1).max(cfg_seq.max(1)),
                    None => cfg_seq.max(1),
                };
                let domain = self.config.validator_identity.domain.as_deref();
                let bytes = vid.sign_manifest(seq, domain).map_err(|e| {
                    NodeError::Config(format!("failed to build local manifest: {e:?}"))
                })?;
                let parsed = rxrpl_overlay::manifest::parse_and_verify(&bytes).map_err(|e| {
                    NodeError::Config(format!(
                        "self-built manifest failed parse_and_verify (bug): {e:?}"
                    ))
                })?;
                if let Err(e) = crate::local_manifest_store::save(
                    data_dir,
                    &crate::local_manifest_store::PersistedManifest {
                        sequence: seq,
                        raw_bytes_hex: hex::encode(&bytes),
                        last_rotated_unix: 0,
                    },
                ) {
                    tracing::warn!(
                        "failed to persist local manifest to {}: {} — node will rebuild on next boot",
                        data_dir.display(),
                        e,
                    );
                }
                tracing::info!(
                    "local manifest built: sequence={}, domain={:?}, prior={:?}",
                    parsed.sequence,
                    domain,
                    persisted.as_ref().map(|p| p.sequence),
                );
                Some(parsed)
            }
            None => None,
        };

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
                        seq,
                        hash,
                        seq + 1
                    );

                    // Bulk-download and VERIFY the full validated state via RPC,
                    // then install it as our base ledger -- the fast, completing
                    // alternative to node-by-node P2P SHAMap sync.
                    if let Some(ref store) = self.node_store {
                        let hash_hex = hex::encode(hash.as_bytes());
                        match Self::fetch_validated_header(rpc_url, &hash_hex).await {
                            Ok(header) => {
                                match Self::download_state_via_rpc(
                                    rpc_url,
                                    &hash_hex,
                                    header.account_hash,
                                    Arc::clone(store),
                                )
                                .await
                                {
                                    Ok(state_map) => {
                                        let mut validated = Ledger::from_catchup_with_store(
                                            seq,
                                            hash,
                                            state_map,
                                            Arc::clone(store),
                                        );
                                        // Apply the trusted validated header so the
                                        // next local close agrees with the chain.
                                        validated.header = header;
                                        *self.ledger.write().await = Ledger::new_open(&validated);
                                        self.closed_ledgers.write().await.push_back(validated);
                                        tracing::info!(
                                            "installed RPC-bootstrapped validated state for ledger #{}",
                                            seq
                                        );
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            "RPC state download failed (P2P sync will be used): {}",
                                            e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "could not fetch validated header for RPC bootstrap: {}",
                                    e
                                );
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
        if let Some(manifest) = local_manifest {
            peer_mgr.set_local_manifest(manifest);
        }

        // 3b. Create overlay event channel for bridging to RPC events
        let (overlay_event_tx, mut overlay_event_rx) =
            tokio::sync::broadcast::channel::<serde_json::Value>(256);
        peer_mgr.set_event_sender(overlay_event_tx);

        // 4. Create relay channel (clone cmd_tx BEFORE moving into adapter)
        let (relay_tx, mut relay_rx) = tokio::sync::mpsc::unbounded_channel::<(Hash256, Vec<u8>)>();
        let cmd_tx_relay = cmd_tx.clone();
        let cmd_tx_catchup = cmd_tx.clone();
        let cmd_tx_rpc = cmd_tx.clone();

        // 5. Create NetworkConsensusAdapter (consumes cmd_tx). When a
        // validator identity is configured, attach it so that proposals are
        // signed with the manifest-bound ephemeral signing key (issue #76 —
        // rippled drops proposals signed with the node peer key as
        // untrusted).
        let adapter = {
            let base = NetworkConsensusAdapter::new(cmd_tx, Arc::clone(&identity));
            match validator_id.as_ref() {
                Some(vid) => base.with_validator_identity(Arc::clone(vid)),
                None => base,
            }
        };

        // 5b. Share the adapter's tx-set cache with the peer manager so it can
        // check for locally known sets and store newly acquired ones.
        peer_mgr.set_tx_sets(Arc::clone(adapter.tx_sets()));

        // 6. Spawn relay bridge: RPC submit -> P2P broadcast
        tokio::spawn(async move {
            while let Some((tx_hash, tx_bytes)) = relay_rx.recv().await {
                tracing::debug!(
                    "relay bridge: forwarding tx {} ({} bytes) to broadcast",
                    tx_hash,
                    tx_bytes.len()
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
        // Peer set: shared with the overlay so `server_info.peers` reads the
        // live connection count.
        ctx.attach_peer_set(peer_mgr.peer_set());
        // Overlay command channel: lets the `connect` admin RPC initiate
        // outbound peer connections via OverlayCommand::ConnectTo.
        ctx.attach_overlay_command(cmd_tx_rpc);
        // Last-close snapshot: populated below by close_consensus_round on
        // each accepted round (proposer count + converge duration). Surfaced
        // as `server_info.last_close`.
        let last_close_arc = Arc::new(std::sync::RwLock::new(
            rxrpl_rpc_server::context::LastCloseSnapshot::default(),
        ));
        ctx.attach_last_close(Arc::clone(&last_close_arc));
        // Network-validated tip: refreshed in the consensus loop when
        // ValidationAggregator reaches UNL quorum for a new seq. Reading
        // this in `server_info` keeps `validated_ledger` honest — it
        // reports what the UNL has agreed on, not what this node has
        // merely closed locally. Stays at default (seq=0) until the first
        // quorum lands.
        let network_validated_arc = Arc::new(std::sync::RwLock::new(
            rxrpl_rpc_server::NetworkValidatedSnapshot::default(),
        ));
        ctx.attach_network_validated(Arc::clone(&network_validated_arc));
        // B5: expose the local manifest to the RPC `manifest` handler.
        if let Some(vid) = validator_id.as_deref() {
            let seq = self.config.validator_identity.sequence.max(1);
            let domain = self.config.validator_identity.domain.clone();
            // Re-build the raw bytes so the RPC server doesn't need
            // overlay's Manifest type. Re-signing is deterministic for
            // the same (master, signing, sequence, domain) tuple.
            match vid.sign_manifest(seq, domain.as_deref()) {
                Ok(raw_bytes) => {
                    let last_rotated_unix =
                        match crate::local_manifest_store::load(&self.config.database.path) {
                            Ok(Some(p)) => p.last_rotated_unix,
                            _ => 0,
                        };
                    let snapshot = rxrpl_rpc_server::LocalManifestSnapshot {
                        master_public_key: vid.master_pubkey().as_bytes().to_vec(),
                        ephemeral_public_key: vid.signing_pubkey().as_bytes().to_vec(),
                        sequence: seq,
                        domain,
                        raw_bytes,
                        last_rotated_unix,
                    };
                    let _ = ctx.set_local_manifest(snapshot);
                }
                Err(e) => {
                    tracing::warn!("failed to build manifest snapshot for RPC: {e:?}");
                }
            }
        }
        let event_tx = ctx.event_sender().clone();

        // Clone ctx for gRPC before moving into RPC router
        #[cfg(feature = "grpc")]
        let grpc_ctx = Arc::clone(&ctx);

        rxrpl_rpc_server::serve(ctx, self.config.server.bind, self.config.server.ws_bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

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
                                        seq: json.get("seq").and_then(|v| v.as_u64()).unwrap_or(0)
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

        // Capture the consensus-message sender before peer_mgr moves into
        // the spawned task; the close path uses it to self-inject our own
        // freshly-signed validations so they count toward UNL quorum.
        let self_validation_tx = peer_mgr.consensus_sender();

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
                } else {
                    hex::decode(trimmed).ok()
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
        let network_validated_for_loop = Arc::clone(&network_validated_arc);
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
        // - Hash(h):  request the header by hash from peers; the anchor is
        //             created when the matching LedgerData arrives (its `seq`
        //             resolves the hash). Tracked in `hash_anchor_pending`.
        let starting_ledger_for_loop = starting_ledger;
        let mut hash_anchor_pending: Option<Hash256> =
            if let Some(crate::checkpoint::StartingLedger::Hash(h)) = starting_ledger_for_loop {
                tracing::info!(
                    "checkpoint bootstrap by hash {}: requesting header from peers",
                    h
                );
                Some(h)
            } else {
                None
            };

        // SECURITY: --starting-ledger guard is enforced at the top of
        // `run_networked` via `validate_starting_ledger_unl` so the error
        // surfaces before any port bind or task spawn.

        // Compute first-close grace before the move closure so we don't
        // capture &self. Grace is 0 when no peers are configured (true solo /
        // single-node sim) so close-time progresses immediately. Otherwise
        // 60s, tuned for cross-impl rippled handshake latency.
        let first_close_grace =
            if self.config.peer.seeds.is_empty() && self.config.peer.fixed_peers.is_empty() {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(60)
            };
        // Pre-extracted before the spawn so the close-time-alignment gate
        // can read it without capturing `self` (which would extend its
        // lifetime past 'static).
        let have_unl_peers_for_loop = !self.config.validators.trusted.is_empty();

        let validator_id_for_loop = validator_id.clone();
        tokio::spawn(async move {
            let node_id = NodeId(identity.node_id);
            // Networked mode: enforce a minimum Establish-phase duration
            // (~rippled `ledgerMIN_CONSENSUS`) so a round is not finalized
            // before peer ProposeSets have had time to propagate. Solo mode
            // leaves this at the default 0 — no peers to wait for.
            // Poll converge() every 250ms (vs the 1250ms proposal cadence)
            // so a round finalizes close to the 1950ms floor instead of
            // overshooting to the next coarse 1250ms tick (~2.5s → ~2.0s).
            let consensus_params = ConsensusParams {
                min_consensus_time_ms: 1_950,
                converge_poll_interval_ms: 250,
                ..ConsensusParams::default()
            };
            let mut timer = ConsensusTimer::new(&consensus_params);
            // Captured before `consensus_params` is moved into the engine;
            // the stale-round early-abandon check needs it to gate the
            // seq-only signal on one full proposal interval.
            let propose_interval_ms = consensus_params.propose_interval_ms;
            // The engine's `public_key` is echoed verbatim into every emitted
            // ProposeSet `node_pub_key`. For UNL-trusted proposals to be
            // counted by rippled, it MUST be the manifest-bound signing key
            // (rippled looks the key up in its trusted-validator set). Use
            // the validator ephemeral signing pubkey when available; fall
            // back to the node pubkey for non-validator operation. Pair with
            // `NetworkConsensusAdapter::with_validator_identity` which
            // signs with the matching private key.
            let consensus_pubkey = match validator_id_for_loop.as_ref() {
                Some(vid) => vid.signing_pubkey().as_bytes().to_vec(),
                None => identity.public_key_bytes().to_vec(),
            };
            // The node knows its own identity definitively — node-peer key,
            // validator master key, and validator ephemeral signing key.
            // Decide ONCE whether we are a trusted UNL member by matching
            // every form against the configured UNL, and tell the engine.
            // The engine cannot do this reliably itself: it signs proposals
            // with the ephemeral key while the UNL may list the master key.
            let self_trusted = {
                let mut t = unl.is_trusted(&node_id);
                if let Some(vid) = validator_id_for_loop.as_ref() {
                    t = t
                        || unl.is_trusted(&NodeId::from_public_key(vid.master_pubkey().as_bytes()))
                        || unl
                            .is_trusted(&NodeId::from_public_key(vid.signing_pubkey().as_bytes()));
                }
                t
            };
            let mut consensus = ConsensusEngine::new_with_unl(
                adapter,
                node_id,
                consensus_pubkey,
                consensus_params,
                unl,
            );
            consensus.set_self_trusted(self_trusted);
            if self_trusted {
                tracing::info!("consensus: node is a trusted UNL validator (counts toward quorum)");
            }

            let mut syncing = false;
            let mut max_peer_seq: u32 = 0;
            let mut pending_close_time = 0u32;
            // Cross-impl bootstrap gate: defer first close until at least one
            // peer has announced its sequence (max_peer_seq > 0). Grace
            // computed at the call site (above the spawn) and captured here
            // because it depends on `self.config` which doesn't escape the
            // closure.
            let mut first_close_completed: bool = false;
            // Marks the wall-clock instant the current open phase started.
            // Set when the round timer enters CloseLedger for a new seq;
            // read on close_consensus_round to populate
            // `server_info.last_close.converge_time_s`.
            let mut round_started_at: Option<tokio::time::Instant> = None;
            let startup_instant = tokio::time::Instant::now();
            // Tracks last observed peer StatusChange. Still updated for
            // diagnostics but no longer gates close (PR2: always-active
            // proposer for mixed-validator support).
            let mut last_peer_status_at: Option<tokio::time::Instant> = None;
            let mut last_round_seq: u32 = 0;
            // Per-round close deferral: while a trusted peer is present but
            // has not yet proposed a close_time for the current seq, hold the
            // local close so rxrpl can adopt the peer's ledger (or close with
            // the peer's close_time bucket) instead of closing solo and
            // diverging. Tracked per seq and bounded by CLOSE_DEFER_MAX so a
            // dead peer never hangs consensus.
            let mut close_defer_seq: u32 = 0;
            let mut close_defer_start: Option<tokio::time::Instant> = None;
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

            let mut pending_validations = crate::pending_validations::PendingValidations::new();

            // Checkpoint bootstrap state (consumed once the anchor resolves).
            let mut checkpoint_anchor: Option<crate::checkpoint::CheckpointAnchor> =
                match starting_ledger_for_loop {
                    Some(crate::checkpoint::StartingLedger::Seq(s)) => {
                        tracing::info!(
                            "checkpoint bootstrap: tracking anchor for ledger #{} (quorum {})",
                            s,
                            initial_quorum
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
            let mut recent_anchor_pending = matches!(
                starting_ledger_for_loop,
                Some(crate::checkpoint::StartingLedger::Recent)
            );

            // Collect amendment votes from received validations for the current round.
            // Reset after each ledger close.
            let mut amendment_votes: Vec<Vec<Hash256>> = Vec::new();
            let mut trusted_validator_count: usize = 0;

            // Cooldown for wrong-prev-ledger recovery to prevent flip-flopping.
            // At most one switch per 10 seconds.
            let mut last_prev_ledger_switch: Option<tokio::time::Instant> = None;
            const PREV_LEDGER_SWITCH_COOLDOWN: Duration = Duration::from_secs(10);

            // Strict thresholds for the seq-only stale-round signal. A peer
            // exactly one ledger ahead is normal cross-impl timing jitter, not
            // a divergent chain — only a gap of >=2 ledgers, on a round that
            // has visibly stalled, against a recently-observed peer status,
            // counts as stale.
            const STALE_SEQ_GAP: u32 = 2;
            const STALE_ROUND_FACTOR: u64 = 4;
            const STALE_PEER_SIGNAL_FRESHNESS: Duration = Duration::from_secs(5);

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
                                    // announced a sequence, capped by first_close_grace for
                                    // solo mode (no peer at all). Grace is 0 when no peers
                                    // are configured at all, so single-node sims close fast.
                                    if !first_close_completed
                                        && max_peer_seq == 0
                                        && startup_instant.elapsed() < first_close_grace
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

                                    // Close-time alignment gate: in mixed-validator topologies,
                                    // each node's `effective_close_time` MUST land in the same
                                    // resolution bucket as rippled's, or the produced ledger
                                    // hash diverges and rxrpl wastes the next ~15s recovering
                                    // via wrong_prev_ledger → catchup → re-close. We anchor
                                    // close_time off `latest_peer_close_time` whenever we have
                                    // a value FOR THIS ROUND; otherwise we fall back to
                                    // wall-clock, which drifts out of bucket whenever rxrpl's
                                    // round fires a few hundred ms before rippled's.
                                    //
                                    // `latest_peer_close_time` is updated on every peer
                                    // proposal regardless of round, so a stale value left over
                                    // from round N-1 equals this round's `parent_close_time`.
                                    // Gating on `.is_some()` alone therefore lets a stale value
                                    // through and rxrpl closes with wall-clock anyway — the
                                    // alternating-divergence bug. Require a value strictly
                                    // greater than `parent_close_time`, which can only come
                                    // from rippled's proposal for the CURRENT round, so rxrpl
                                    // always lands in rippled's exact bucket and produces a
                                    // matching ledger every round. rxrpl still proposes (just a
                                    // few hundred ms later) so it stays an active proposer.
                                    //
                                    // The cap matches `first_close_grace` so a stuck network
                                    // still progresses — after that, we fall through and close
                                    // solo, accepting the divergence as the lesser evil vs.
                                    // hanging consensus indefinitely.
                                    let have_fresh_peer_ct = consensus
                                        .latest_peer_close_time()
                                        .map(|ct| ct > parent_close_time)
                                        .unwrap_or(false);
                                    // Persistent per-round deferral. Each round, hold the
                                    // local close until a trusted peer has proposed a
                                    // close_time for THIS seq — then rxrpl closes in the
                                    // peer's bucket (or has already adopted the peer's
                                    // ledger via catchup). Bounded by CLOSE_DEFER_MAX: a
                                    // silent/dead peer still lets rxrpl close solo rather
                                    // than hang. This replaces the startup-only deferral
                                    // that stopped firing after first_close_grace and let
                                    // rxrpl close every steady-state ledger solo.
                                    const CLOSE_DEFER_MAX: Duration = Duration::from_secs(25);
                                    if close_defer_seq != seq {
                                        close_defer_seq = seq;
                                        close_defer_start = Some(tokio::time::Instant::now());
                                    }
                                    let deferred_for = close_defer_start
                                        .map(|t| t.elapsed())
                                        .unwrap_or_default();
                                    // If a trusted peer has already proposed a ledger past
                                    // our open seq, rxrpl is behind: closing #N solo here
                                    // would pick a future-round `latest_peer_close_time`
                                    // and diverge. Defer unconditionally — the catchup path
                                    // adopts #N from the peer and advances our seq.
                                    let peer_ahead = have_unl_peers_for_loop
                                        && consensus
                                            .latest_peer_ledger_seq()
                                            .is_some_and(|s| s > seq);
                                    if peer_ahead {
                                        tracing::debug!(
                                            target: "consensus",
                                            seq,
                                            "deferring close: peer is ahead, awaiting catchup adopt"
                                        );
                                        timer.on_phase_change(
                                            rxrpl_consensus::ConsensusPhase::Open,
                                        );
                                        continue;
                                    }
                                    if have_unl_peers_for_loop
                                        && !have_fresh_peer_ct
                                        && deferred_for < CLOSE_DEFER_MAX
                                    {
                                        tracing::debug!(
                                            target: "consensus",
                                            seq,
                                            deferred_s = deferred_for.as_secs(),
                                            "deferring close: awaiting peer ProposeSet for this round"
                                        );
                                        timer.on_phase_change(
                                            rxrpl_consensus::ConsensusPhase::Open,
                                        );
                                        continue;
                                    }

                                    if last_round_seq != seq {
                                        last_round_seq = seq;
                                        round_started_at = Some(tokio::time::Instant::now());
                                    }

                                    // Propose our own wall-clock floored to the resolution
                                    // grid. Using `latest_peer_close_time` here was a
                                    // cross-impl footgun: that value spans ALL rounds, so
                                    // when rxrpl is even slightly behind it carries
                                    // rippled's close_time for a *future* ledger — already
                                    // 10s+ past this round's bucket — and forks the hash
                                    // every round. The engine's `align_close_time_with_peers`
                                    // (inside `converge()`) handles within-round bucket
                                    // alignment from the peer's current-round proposal; and
                                    // the no-consensus fallback in `effective_close_time`
                                    // (→ `parent + 1` without a strict majority) keeps a
                                    // 1-bucket split converging byte-for-byte cross-impl.
                                    let resolution = consensus.close_time_resolution();
                                    let raw_close_time = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        .saturating_sub(rxrpl_ledger::header::RIPPLE_EPOCH_OFFSET)
                                        as u32;
                                    let own_bucket = match raw_close_time.checked_div(resolution) {
                                        Some(q) => q * resolution,
                                        None => raw_close_time,
                                    }
                                    .max(parent_close_time.saturating_add(1));

                                    // Adopt the peer's close-time bucket when a trusted
                                    // peer has proposed a close_time for THIS round
                                    // (`have_fresh_peer_ct`). At the Open→Establish close
                                    // boundary the engine's `peer_positions` is still
                                    // empty (it only fills during `converge()`), so
                                    // `effective_close_time` takes the solo path and we
                                    // close with our own wall-clock bucket. The
                                    // CT_SKEW_DUMP shows `our_close_time` vs
                                    // `latest_peer_ct` differing by exactly one 10s
                                    // resolution step every round → a 1-bucket fork that
                                    // costs a wrong_prev → catchup recovery before
                                    // reconverging. Flooring the peer's already-fresh
                                    // close_time to our resolution lands us in rippled's
                                    // bucket deterministically. The historical "footgun"
                                    // (a stale cross-round value carrying a future
                                    // ledger's time) is now guarded by the `peer_ahead`
                                    // deferral above: if the peer is past our seq we
                                    // defer instead of closing, so a fresh peer ct here
                                    // always belongs to the current round.
                                    let close_time = if have_fresh_peer_ct {
                                        consensus
                                            .latest_peer_close_time()
                                            .map(|pct| {
                                                match pct.checked_div(resolution) {
                                                    Some(q) => q * resolution,
                                                    None => pct,
                                                }
                                                .max(parent_close_time.saturating_add(1))
                                            })
                                            .unwrap_or(own_bucket)
                                    } else {
                                        own_bucket
                                    };

                                    // Always-active proposer: close on schedule like rippled.
                                    // Previous deferrals (peer_at_or_past, peer_behind_alive) kept
                                    // rxrpl in passive validator mode whenever any peer was
                                    // observable, so consensus.close_ledger() never fired and no
                                    // ProposeSet was broadcast (issue #76). Removing them lets
                                    // rxrpl participate as an active proposer in mixed-validator
                                    // topologies. Convergence on divergent peer chains is handled
                                    // by the existing wrong_prev_ledger / catchup recovery path.
                                    tracing::info!(
                                        "closing seq={} prev={} peer_seq={}",
                                        seq, prev_hash, max_peer_seq
                                    );

                                    // DEBUG close-time skew diagnosis: capture the
                                    // cross-round vs per-round signals at the close
                                    // decision point. Confirms whether rxrpl closes
                                    // with a fresh latest_peer_close_time (cross-round)
                                    // but zero peer_positions for THIS round (per-round)
                                    // — the 1-bucket transient fork hypothesis.
                                    tracing::debug!(
                                        target: "consensus",
                                        "CT_SKEW_DUMP seq={} our_close_time={} resolution={} parent_close_time={} latest_peer_ct={:?} peer_pos_count={} have_fresh_peer_ct={} deferred_s={}",
                                        seq,
                                        close_time,
                                        resolution,
                                        parent_close_time,
                                        consensus.latest_peer_close_time(),
                                        consensus.peer_position_count(),
                                        have_fresh_peer_ct,
                                        deferred_for.as_secs(),
                                    );

                                    let l = ledger.read().await;
                                    let tx_set = Node::collect_consensus_tx_set(&l);
                                    drop(l);

                                    // Pass parent_close_time so the engine's
                                    // eff_close_time monotonicity clamp lands at
                                    // parent+1 — matching rippled. With the legacy
                                    // start_round (prior=0), the clamp is inactive
                                    // and rxrpl forks 1 bucket from rippled every
                                    // round (CT_DUMP empirical, 2026-05-21).
                                    consensus.start_round_with_prior(prev_hash, seq, parent_close_time);
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
                                        let proposers_this_round =
                                            consensus.peer_position_count() as u32;
                                        Self::close_consensus_round(
                                            &mut consensus, pending_close_time, &ledger,
                                            &closed_ledgers, &tx_store, &event_tx,
                                            &ledger_seq_shared, &ledger_hash_shared,
                                            &tx_queue, &identity, &validator_id_for_loop,
                                            &cmd_tx_catchup,
                                            &amendment_table, &tx_engine, &fees,
                                            &amendment_votes, trusted_validator_count,
                                            &pruner, &node_store,
                                            &self_validation_tx,
                                        ).await;
                                        if let Ok(mut lc) = last_close_arc.write() {
                                            lc.proposers = proposers_this_round;
                                            lc.converge_time_s = round_started_at
                                                .map(|t| t.elapsed().as_secs_f64())
                                                .unwrap_or(0.0);
                                        }
                                        round_started_at = None;
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
                                    // Early-abandon of a stale round. If a trusted peer is
                                    // provably ahead of us, the in-progress Establish round
                                    // can NEVER reach quorum: every peer ProposeSet references
                                    // a future prev_ledger and lands in the engine's
                                    // `future_proposals` holding pen instead of being counted.
                                    // Left alone, `converge()` burns all `max_consensus_rounds`
                                    // (25 × 1250ms ≈ 31s) before force-accepting a ledger that
                                    // diverges, then recovers via wrong_prev_ledger → catchup —
                                    // an infinite ~31-38s/ledger loop in a mixed network.
                                    //
                                    // Detect "behind" from two independent signals and bail
                                    // into catchup within ~1 round instead:
                                    //  1. `check_wrong_prev_ledger()` — a trusted supermajority
                                    //     has signed/proposed a different prev_ledger.
                                    //  2. `max_peer_seq >= our_open_seq + STALE_SEQ_GAP` — a
                                    //     peer announced a ledger at least two ahead of the one
                                    //     we are still trying to close.
                                    //
                                    // False-positive guards:
                                    //  - The whole block is gated on `check_wrong_prev_ledger`
                                    //    returning Some OR the seq signal; solo mode (empty UNL
                                    //    → `check_wrong_prev_ledger` is None, `max_peer_seq`
                                    //    stays 0) never trips it.
                                    //  - The seq-only signal is deliberately strict: `max_peer_seq`
                                    //    is a monotonic high-water mark and rises by one whenever
                                    //    any peer closes a ledger slightly before us — normal
                                    //    cross-impl jitter. A gap of exactly 1 is therefore
                                    //    ignored; only a gap of >= STALE_SEQ_GAP, on a round that
                                    //    has run longer than STALE_ROUND_FACTOR propose intervals
                                    //    (a healthy round converges well within that), and backed
                                    //    by a peer StatusChange seen within STALE_PEER_SIGNAL_-
                                    //    FRESHNESS, counts as stale. `check_wrong_prev_ledger`
                                    //    is a hard signal (a trusted set already disagreed) and
                                    //    needs no delay.
                                    //  - Shares the `PREV_LEDGER_SWITCH_COOLDOWN` with the
                                    //    proposal-path recovery so the two cannot thrash.
                                    let our_open_seq =
                                        ledger_seq_shared.load(Ordering::Relaxed);
                                    let cooldown_ok = last_prev_ledger_switch
                                        .map(|t| t.elapsed() >= PREV_LEDGER_SWITCH_COOLDOWN)
                                        .unwrap_or(true);
                                    let wrong_prev =
                                        consensus.check_wrong_prev_ledger();
                                    let round_stalled = round_started_at
                                        .map(|t| {
                                            t.elapsed().as_millis() as u64
                                                > propose_interval_ms
                                                    .saturating_mul(STALE_ROUND_FACTOR)
                                        })
                                        .unwrap_or(false);
                                    let peer_signal_fresh = last_peer_status_at
                                        .map(|t| {
                                            t.elapsed() < STALE_PEER_SIGNAL_FRESHNESS
                                        })
                                        .unwrap_or(false);
                                    let behind_by_seq = max_peer_seq
                                        >= our_open_seq.saturating_add(STALE_SEQ_GAP)
                                        && round_stalled
                                        && peer_signal_fresh;
                                    // Suppress wrong_prev when the "preferred" alternative is a
                                    // ledger we already own. That happens routinely on 2-node
                                    // bootstraps: peer A closes seq N a moment before peer B, so
                                    // B's still-establishing proposals reference prev = hash_of_(N-1)
                                    // while A is already proposing for seq N+1 with prev = hash_of_N.
                                    // A's `check_wrong_prev_ledger` then sees 100% trusted support
                                    // for `hash_of_(N-1)`, but that hash is *behind* us, not a
                                    // competing chain — A already has it in `closed_ledgers`.
                                    // Catching up to a ledger we already own forces a round trip
                                    // every other consensus round, producing ~30s/ledger ping-pong.
                                    let wrong_prev_is_known_history = match wrong_prev.as_ref() {
                                        Some(detected) => {
                                            let history = closed_ledgers.read().await;
                                            history
                                                .iter()
                                                .any(|l| l.header.hash == detected.preferred_ledger)
                                        }
                                        None => false,
                                    };
                                    let wrong_prev_trips =
                                        wrong_prev.is_some() && !wrong_prev_is_known_history;
                                    if cooldown_ok
                                        && consensus.phase()
                                            == rxrpl_consensus::ConsensusPhase::Establish
                                        && (wrong_prev_trips || behind_by_seq)
                                    {
                                        tracing::warn!(
                                            wrong_prev = wrong_prev.is_some(),
                                            max_peer_seq,
                                            our_open_seq,
                                            "abandoning stale consensus round: trusted peer is \
                                             ahead, entering catchup instead of exhausting \
                                             max_consensus_rounds"
                                        );
                                        last_prev_ledger_switch =
                                            Some(tokio::time::Instant::now());
                                        syncing = true;
                                        timer.on_phase_change(
                                            rxrpl_consensus::ConsensusPhase::Open,
                                        );
                                        sync_started_at =
                                            Some(tokio::time::Instant::now());

                                        // Walk the contiguous run from the OLDEST held
                                        // ledger and request the lowest gap, so the
                                        // forward-chain LedgerData adopt path fills every
                                        // intermediate ledger up to the peer tip. Mirrors
                                        // the proposal-path recovery block below.
                                        let highest_contiguous = {
                                            let history = closed_ledgers.read().await;
                                            let mut iter = history.iter();
                                            match iter.next() {
                                                None => 0,
                                                Some(first) => {
                                                    let mut hc =
                                                        first.header.sequence;
                                                    for l in iter {
                                                        if l.header.sequence
                                                            == hc + 1
                                                        {
                                                            hc = l.header
                                                                .sequence;
                                                        } else {
                                                            break;
                                                        }
                                                    }
                                                    hc
                                                }
                                            }
                                        };
                                        let lowest_missing = highest_contiguous + 1;
                                        last_sync_seq = lowest_missing;
                                        let _ = cmd_tx_catchup.send(
                                            OverlayCommand::RequestLedger {
                                                seq: lowest_missing,
                                                hash: None,
                                            },
                                        );

                                        // Reset the engine off the stale round. When
                                        // `check_wrong_prev_ledger` named a preferred
                                        // ledger, anchor on it; otherwise keep our
                                        // prev_ledger and let catchup advance us.
                                        let reset_prev = wrong_prev
                                            .map(|d| d.preferred_ledger)
                                            .unwrap_or_else(|| consensus.prev_ledger());
                                        consensus.start_round(reset_prev, 0);
                                        amendment_votes.clear();
                                        trusted_validator_count = 0;
                                        round_started_at = None;
                                        continue;
                                    }

                                    if consensus.converge() {
                                        timer.on_phase_change(consensus.phase());
                                        let _ = event_tx.send(ServerEvent::ConsensusPhaseChange {
                                            phase: "accepted".into(),
                                        });
                                        let proposers_this_round =
                                            consensus.peer_position_count() as u32;
                                        Self::close_consensus_round(
                                            &mut consensus, pending_close_time, &ledger,
                                            &closed_ledgers, &tx_store, &event_tx,
                                            &ledger_seq_shared, &ledger_hash_shared,
                                            &tx_queue, &identity, &validator_id_for_loop,
                                            &cmd_tx_catchup,
                                            &amendment_table, &tx_engine, &fees,
                                            &amendment_votes, trusted_validator_count,
                                            &pruner, &node_store,
                                            &self_validation_tx,
                                        ).await;
                                        if let Ok(mut lc) = last_close_arc.write() {
                                            lc.proposers = proposers_this_round;
                                            lc.converge_time_s = round_started_at
                                                .map(|t| t.elapsed().as_secs_f64())
                                                .unwrap_or(0.0);
                                        }
                                        round_started_at = None;
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
                                            // Abandon round, re-open same ledger.
                                            // Pass parent_close_time so eff_close_time
                                            // keeps the monotonicity clamp active.
                                            let l = ledger.read().await;
                                            consensus.start_round_with_prior(
                                                l.header.parent_hash,
                                                l.header.sequence,
                                                l.header.parent_close_time,
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
                                // Capture the proposed round seq before the
                                // proposal is moved into the engine; recovery
                                // below needs it to size the catchup target.
                                let proposal_ledger_seq = proposal.ledger_seq;
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

                                        // The detected preferred_ledger is the prev_ledger
                                        // of the round the peer is proposing, so its seq is
                                        // `proposal.ledger_seq - 1`. Bump max_peer_seq to it
                                        // so the forward-chain (LedgerData adopt path) walks
                                        // all the way up to the preferred tip.
                                        let preferred_seq =
                                            proposal_ledger_seq.saturating_sub(1);
                                        if preferred_seq > max_peer_seq {
                                            max_peer_seq = preferred_seq;
                                        }

                                        // Request the LOWEST missing ledger by seq, not the
                                        // preferred tip by hash. Requesting the tip directly
                                        // makes the forward-chain walk UPWARD from the tip
                                        // only, leaving the intermediate ledgers between our
                                        // last-held seq and the tip permanently unrequested
                                        // (the complete_ledgers holes). Starting from the
                                        // lowest gap lets the existing forward-chain fill
                                        // every ledger contiguously up to `target`
                                        // (== max_peer_seq, now the preferred tip).
                                        // Contiguous run from the OLDEST held ledger — not
                                        // from seq 1 — so a pruned history (online_delete)
                                        // starting above seq 1 is handled correctly instead
                                        // of re-requesting from genesis.
                                        let highest_contiguous = {
                                            let history = closed_ledgers.read().await;
                                            let mut iter = history.iter();
                                            match iter.next() {
                                                None => 0,
                                                Some(first) => {
                                                    let mut hc = first.header.sequence;
                                                    for l in iter {
                                                        if l.header.sequence == hc + 1 {
                                                            hc = l.header.sequence;
                                                        } else {
                                                            break;
                                                        }
                                                    }
                                                    hc
                                                }
                                            }
                                        };
                                        let lowest_missing = highest_contiguous + 1;
                                        last_sync_seq = lowest_missing;
                                        let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                            seq: lowest_missing,
                                            hash: None,
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

                                // Plumb the validation into the consensus
                                // engine's negative-UNL tracker (C-B6).
                                Node::record_validation_into_engine(
                                    &mut consensus,
                                    &validation,
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

                                if !anchor_trustable {
                                    pending_validations.buffer(
                                        validation,
                                        std::time::Instant::now(),
                                    );
                                    tracing::debug!(
                                        target: "consensus",
                                        buffered = pending_validations.len(),
                                        "validation buffered (signing key not yet trusted)"
                                    );
                                    continue;
                                }

                                // Aggregate validation and check for quorum
                                if let Some(validated) = val_aggregator.add_validation(validation) {
                                    tracing::info!(
                                        "network validated ledger #{} hash={} ({} validations)",
                                        validated.seq, validated.hash, validated.validation_count
                                    );
                                    let our_seq = ledger_seq_shared.load(Ordering::Relaxed);

                                    // Look up the matching closed ledger so the
                                    // network-validated snapshot and the
                                    // `LedgerClosed` event carry the actual
                                    // close_time + tx count. Peers may validate
                                    // a hash we haven't reconstructed yet — in
                                    // that case we still publish the seq/hash
                                    // (close_time=0) so dashboards see the
                                    // network is ahead; the proper fields fill
                                    // in once catchup adopts the matching ledger.
                                    let (close_time, txn_count) = {
                                        let history = closed_ledgers.read().await;
                                        history
                                            .iter()
                                            .find(|l| {
                                                l.header.sequence == validated.seq
                                                    && l.header.hash == validated.hash
                                            })
                                            .map(|l| {
                                                let mut c = 0u32;
                                                l.tx_map.for_each(&mut |_, _| c += 1);
                                                (l.header.close_time, c)
                                            })
                                            .unwrap_or((0u32, 0u32))
                                    };

                                    // Publish the validated tip so `server_info`
                                    // can report it as `validated_ledger` and
                                    // cap `complete_ledgers`. Monotone advance
                                    // only — `add_validation` already returns
                                    // Some at most once per (seq,hash), but a
                                    // future cleanup could relax that, and the
                                    // monotone guard makes this future-proof.
                                    if let Ok(mut guard) = network_validated_for_loop.write() {
                                        if validated.seq > guard.seq {
                                            guard.seq = validated.seq;
                                            guard.hash = validated.hash;
                                            guard.close_time = close_time;
                                            // Emit the network-validated
                                            // `LedgerClosed` event. This is the
                                            // single source of truth for the
                                            // `ledger` subscribe stream in
                                            // networked mode; the local-close
                                            // path no longer emits.
                                            let _ = event_tx.send(ServerEvent::LedgerClosed {
                                                ledger_index: validated.seq,
                                                ledger_hash: validated.hash,
                                                ledger_time: close_time,
                                                txn_count,
                                            });
                                        }
                                    }

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
                                // Hash bootstrap: while unresolved, ask this peer
                                // for the target ledger by hash. The `seq` hint is
                                // the peer's tip so a peer is selectable; the server
                                // resolves by hash and the reply's real `seq` lands
                                // the anchor in the LedgerData handler below.
                                if let Some(h) = hash_anchor_pending {
                                    let _ = cmd_tx_catchup.send(OverlayCommand::RequestLedger {
                                        seq: peer_seq,
                                        hash: Some(h),
                                    });
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
                                // Hash bootstrap resolution: the requested hash now
                                // maps to a concrete seq, so anchor the checkpoint
                                // there (same shape as the Seq path).
                                if hash_anchor_pending == Some(hash) && checkpoint_anchor.is_none() {
                                    tracing::info!(
                                        "checkpoint bootstrap (hash): resolved {} to ledger #{}",
                                        hash, seq
                                    );
                                    checkpoint_anchor = Some(crate::checkpoint::CheckpointAnchor::new(
                                        crate::checkpoint::AnchorConfig {
                                            target_seq: seq,
                                            quorum: initial_quorum,
                                        },
                                    ));
                                    hash_anchor_pending = None;
                                }
                                if !nodes.is_empty() {
                                    let cached = catchup_headers.get(&seq);
                                    match Node::try_reconstruct_ledger(seq, hash, &nodes, &node_store, cached) {
                                        Ok(reconstructed) => {
                                            let mut history = closed_ledgers.write().await;
                                            // Diagnostic: a reconstructed peer ledger #N carries
                                            // the peer's #(N-1) hash + close_time as its parent_*
                                            // fields. If we locally closed #(N-1) to a different
                                            // hash, log which header field diverged so the
                                            // cross-impl close mismatch can be pinpointed.
                                            if let Some(local_parent) = history
                                                .iter()
                                                .find(|l| l.header.sequence + 1 == seq)
                                            {
                                                let lp = &local_parent.header;
                                                if lp.hash != reconstructed.header.parent_hash {
                                                    let ct_note = if lp.close_time
                                                        != reconstructed.header.parent_close_time
                                                    {
                                                        format!(
                                                            "close_time {} vs {}",
                                                            lp.close_time,
                                                            reconstructed.header.parent_close_time
                                                        )
                                                    } else {
                                                        "close_time identical (divergence in tx_hash/account_hash/flags)".to_string()
                                                    };
                                                    tracing::warn!(
                                                        "catchup: local #{} {} diverges from peer parent {}; {}",
                                                        lp.sequence,
                                                        lp.hash,
                                                        reconstructed.header.parent_hash,
                                                        ct_note
                                                    );
                                                }
                                            }
                                            // REPLACE on hash mismatch: if we already have a
                                            // locally-closed ledger at this seq with a different
                                            // hash, the catchup-reconstructed copy is canonical
                                            // (a trusted UNL peer references it, we just
                                            // confirmed via liBASE+state delta). Keeping the
                                            // local divergent copy means RPC `ledger` queries
                                            // return the wrong hash and the next ledger's
                                            // skip-list references the wrong parent.
                                            if let Some(existing_idx) =
                                                history.iter().position(|l| l.header.sequence == seq)
                                            {
                                                if history[existing_idx].header.hash != reconstructed.header.hash {
                                                    tracing::info!(
                                                        "catchup: replacing diverged local ledger #{} (was {}, now {})",
                                                        seq,
                                                        history[existing_idx].header.hash,
                                                        reconstructed.header.hash,
                                                    );
                                                    let lo = &history[existing_idx].header;
                                                    let hi = &reconstructed.header;
                                                    let mut diff: Vec<String> = Vec::new();
                                                    if lo.parent_hash != hi.parent_hash {
                                                        diff.push(format!("parent_hash {} vs {}", lo.parent_hash, hi.parent_hash));
                                                    }
                                                    if lo.tx_hash != hi.tx_hash {
                                                        diff.push(format!("tx_hash {} vs {}", lo.tx_hash, hi.tx_hash));
                                                    }
                                                    if lo.account_hash != hi.account_hash {
                                                        diff.push(format!("account_hash {} vs {}", lo.account_hash, hi.account_hash));
                                                    }
                                                    if lo.drops != hi.drops {
                                                        diff.push(format!("drops {} vs {}", lo.drops, hi.drops));
                                                    }
                                                    if lo.parent_close_time != hi.parent_close_time {
                                                        diff.push(format!("parent_close_time {} vs {}", lo.parent_close_time, hi.parent_close_time));
                                                    }
                                                    if lo.close_time != hi.close_time {
                                                        diff.push(format!("close_time {} vs {}", lo.close_time, hi.close_time));
                                                    }
                                                    if lo.close_time_resolution != hi.close_time_resolution {
                                                        diff.push(format!("close_time_resolution {} vs {}", lo.close_time_resolution, hi.close_time_resolution));
                                                    }
                                                    if lo.close_flags != hi.close_flags {
                                                        diff.push(format!("close_flags {} vs {}", lo.close_flags, hi.close_flags));
                                                    }
                                                    tracing::warn!(
                                                        "catchup: header divergence #{} fields: [{}]",
                                                        seq,
                                                        diff.join("; ")
                                                    );
                                                    history[existing_idx] = reconstructed.clone();
                                                } else {
                                                    tracing::debug!(
                                                        "catchup: ledger #{} already present with matching hash",
                                                        seq
                                                    );
                                                }
                                            } else {
                                                let pos = history.partition_point(|l| l.header.sequence < seq);
                                                history.insert(pos, reconstructed.clone());
                                                while history.len() > crate::consensus_adapter::MAX_CLOSED_LEDGERS {
                                                    history.pop_front();
                                                }
                                                tracing::info!("catchup: reconstructed ledger #{} hash={}", seq, hash);
                                            }

                                            // Contiguity backfill: a catchup that jumps
                                            // straight to a peer's tip (any trigger path —
                                            // wrong_prev recovery, peer-tip yield,
                                            // StatusChange) leaves the ledgers between our
                                            // last-held seq and the tip permanently
                                            // unrequested — the holes in
                                            // `closed_ledgers` / `complete_ledgers`.
                                            //
                                            // Fix at the single common adopt site: if the
                                            // ledger directly below the one just
                                            // reconstructed is missing, request it by hash
                                            // (= this ledger's `parent_hash`). The
                                            // reconstructed parent re-enters this block and
                                            // requests ITS parent, walking the hash chain
                                            // downward until it reaches a ledger we already
                                            // hold or genesis. Bounded to the
                                            // MAX_CLOSED_LEDGERS retention window — a deeper
                                            // request would be `pop_front`-evicted on
                                            // insert anyway.
                                            if seq > 1 {
                                                // A locally-closed ledger at seq-1 whose hash does
                                                // not match the reconstructed child's parent_hash
                                                // is a DIVERGENT parent — we must still request the
                                                // peer's seq-1 so the catchup replace branch
                                                // (canonical replacement + field-by-field diag)
                                                // runs. Without this, a diverged local close
                                                // silently stays in `closed_ledgers` and the
                                                // skip-list / RPC return the wrong hash.
                                                let parent_hash = reconstructed.header.parent_hash;
                                                let have_parent = history.iter().any(|l| {
                                                    l.header.sequence == seq - 1
                                                        && l.header.hash == parent_hash
                                                });
                                                let tip_seq = history
                                                    .iter()
                                                    .next_back()
                                                    .map(|l| l.header.sequence)
                                                    .unwrap_or(seq);
                                                let within_window = tip_seq.saturating_sub(seq - 1)
                                                    < crate::consensus_adapter::MAX_CLOSED_LEDGERS
                                                        as u32;
                                                if !have_parent
                                                    && within_window
                                                    && parent_hash != Hash256::ZERO
                                                {
                                                    let _ = cmd_tx_catchup.send(
                                                        OverlayCommand::RequestLedger {
                                                            seq: seq - 1,
                                                            hash: Some(parent_hash),
                                                        },
                                                    );
                                                    tracing::debug!(
                                                        "catchup: backfilling missing parent #{} (hash={})",
                                                        seq - 1,
                                                        parent_hash
                                                    );
                                                }
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

                                                // Surface the catchup-adopted ledger on the WS
                                                // event stream so dashboards / subscribers tracking
                                                // `ledgerClosed` see the new validated tip. Without
                                                // this they stall on the previous event (typically
                                                // the genesis ledger), even though `complete_ledgers`
                                                // and `validated_ledger` keep advancing via HTTP.
                                                let _ = event_tx.send(ServerEvent::LedgerClosed {
                                                    ledger_index: reconstructed.header.sequence,
                                                    ledger_hash: reconstructed.header.hash,
                                                    ledger_time: reconstructed.header.close_time,
                                                    txn_count: 0,
                                                });

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
                                                    let signing_pubkey: Vec<u8> = match validator_id_for_loop.as_ref() {
                                                        Some(vid) => vid.signing_pubkey().as_bytes().to_vec(),
                                                        None => identity.public_key_bytes().to_vec(),
                                                    };
                                                    let mut validation = Validation {
                                                        node_id: rxrpl_consensus::types::NodeId(Hash256::new(identity.node_id.0)),
                                                        public_key: signing_pubkey.clone(),
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
                                                    if let Some(ref vid) = validator_id_for_loop {
                                                        vid.sign_validation(&mut validation);
                                                    } else {
                                                        identity.sign_validation(&mut validation);
                                                    }
                                                    let payload = rxrpl_overlay::proto_convert::encode_validation(
                                                        &validation,
                                                        &signing_pubkey,
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

                                if !revoked {
                                    if let Some(ref eph) = ephemeral_key {
                                        let drained = pending_validations
                                            .drain(eph.as_bytes(), std::time::Instant::now());
                                        if !drained.is_empty() {
                                            tracing::debug!(
                                                target: "consensus",
                                                ephemeral = %eph,
                                                replayed = drained.len(),
                                                replayed_total = pending_validations.replayed_total(),
                                                "replaying buffered validations after manifest"
                                            );
                                            for v in drained {
                                                let _ = val_aggregator.add_validation(v);
                                            }
                                        }
                                    }
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
            "networked node running (close interval: {}s), waiting for SIGINT/SIGTERM",
            close_interval_secs
        );

        let signal = crate::shutdown::wait_for_shutdown()
            .await
            .map_err(|e| NodeError::Server(format!("signal error: {e}")))?;

        tracing::info!(signal = %signal, "shutting down gracefully");
        Ok(())
    }

    /// Close a consensus round: apply the accepted set, close ledger, emit events.
    #[allow(clippy::too_many_arguments)]
    async fn close_consensus_round<A: rxrpl_consensus::ConsensusAdapter>(
        consensus: &mut ConsensusEngine<A>,
        pending_close_time: u32,
        ledger: &Arc<RwLock<Ledger>>,
        closed_ledgers: &Arc<RwLock<VecDeque<Ledger>>>,
        tx_store: &Option<Arc<dyn TxStore>>,
        event_tx: &tokio::sync::broadcast::Sender<ServerEvent>,
        ledger_seq_shared: &Arc<AtomicU32>,
        ledger_hash_shared: &Arc<tokio::sync::RwLock<Hash256>>,
        tx_queue: &Arc<RwLock<TxQueue>>,
        identity: &Arc<NodeIdentity>,
        validator_id_for_loop: &Option<Arc<rxrpl_overlay::identity::ValidatorIdentity>>,
        cmd_tx: &tokio::sync::mpsc::UnboundedSender<OverlayCommand>,
        amendment_table: &Arc<RwLock<AmendmentTable>>,
        tx_engine: &Arc<TxEngine>,
        fees: &Arc<FeeSettings>,
        validator_amendment_votes: &[Vec<Hash256>],
        trusted_validator_count: usize,
        pruner: &Arc<LedgerPruner>,
        node_store: &Option<Arc<dyn NodeStore>>,
        self_validation_tx: &tokio::sync::mpsc::UnboundedSender<rxrpl_overlay::ConsensusMessage>,
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
        let mut l = ledger.write().await;
        let parent_close_time = l.header.parent_close_time;
        let resolution = consensus.adaptive_close_time().resolution();
        // Prefer the engine's converged value; the `latest_peer_close_time`
        // fallback was a cross-impl footgun — it picked the peer's close_time
        // from any round, including future ones, when rxrpl was behind. Fall
        // back to rippled's no-consensus value (`eff_close_time(0,...)` =
        // `parent + 1`) instead so the final close stays deterministic.
        let raw_close_time = consensus
            .accepted_close_time()
            .or_else(|| consensus.rounded_close_time())
            .unwrap_or_else(|| rxrpl_consensus::round_close_time(pending_close_time, resolution));
        // Clamp `close_time > parent_close_time` to mirror rippled's
        // `effCloseTime` (xrpld/consensus/LedgerTiming.h). Without this,
        // two consecutive ledgers that close within the same resolution
        // bucket get equal close_time → equal ledger headers but
        // different account_hash on the next round → unrecoverable
        // divergence from rippled. See
        // docs/superpowers/specs/2026-05-14-close-time-monotonicity-fix.md.
        let effective_close_time =
            rxrpl_consensus::eff_close_time(raw_close_time, resolution, parent_close_time);
        tracing::debug!(
            "closing with effective_close_time={} close_flags={} pending_close_time={} parent_close_time={}",
            effective_close_time,
            close_flags,
            pending_close_time,
            parent_close_time,
        );

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

        // Apply negative-UNL pseudo-transactions on flag ledgers.
        let nunl_seq = l.header.sequence;
        let _nunl_results = Node::apply_negative_unl(consensus, &mut l, tx_engine, fees, nunl_seq);
        consensus.on_ledger_close_for_tracker();

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

        // NOTE: ServerEvent::LedgerClosed is intentionally NOT emitted here.
        // In networked mode, local close is just the first vote — the round
        // is only "closed" from the network's perspective once UNL quorum
        // is reached. The emit happens in the `ConsensusMessage::Validation`
        // handler when `ValidationAggregator` returns the first
        // quorum-reaching validation for this seq. That keeps the
        // `validated_ledger` field and the `ledger` subscribe stream honest
        // about what the network has actually agreed on (rather than what
        // this node has merely closed locally).

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
            let signing_pubkey: Vec<u8> = match validator_id_for_loop.as_ref() {
                Some(vid) => vid.signing_pubkey().as_bytes().to_vec(),
                None => identity.public_key_bytes().to_vec(),
            };
            let mut validation = Validation {
                node_id: rxrpl_consensus::types::NodeId(Hash256::new(identity.node_id.0)),
                public_key: signing_pubkey.clone(),
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
            if let Some(vid) = validator_id_for_loop.as_ref() {
                vid.sign_validation(&mut validation);
            } else {
                identity.sign_validation(&mut validation);
            }
            let payload =
                rxrpl_overlay::proto_convert::encode_validation(&validation, &signing_pubkey);
            let _ = cmd_tx.send(OverlayCommand::Broadcast {
                msg_type: rxrpl_p2p_proto::MessageType::Validation,
                payload,
            });

            // Self-inject the same validation into our own consensus loop so
            // it counts toward UNL quorum locally. Peer broadcast does not
            // loop back through `consensus_tx`, so without this our
            // aggregator would max out at (UNL_size - 1) votes per ledger
            // and never reach quorum=ceil(N*0.8) when N=4 (mixed kurtosis:
            // 2 rxrpl + 2 rippled). Rippled does the equivalent in
            // `Validations::add` from its own onAccept path.
            let _ =
                self_validation_tx.send(rxrpl_overlay::ConsensusMessage::Validation(validation));
        }

        // Broadcast StatusChange so peers know our current ledger and our
        // complete-ledger range. Advertising (1, closed_seq) lets late-joining
        // rippled peers know rxrpl can serve all ancestors during catchup;
        // without that range, rippled never asks for them and its
        // `complete_ledgers` stays empty.
        {
            let payload = rxrpl_overlay::proto_convert::encode_status_change(
                &hash, closed_seq, 1, closed_seq,
            );
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

                let old: Vec<_> = history
                    .iter()
                    .filter(|l| l.header.sequence <= cutoff_seq)
                    .cloned()
                    .collect();

                let retained = history.iter().find(|l| l.header.sequence > cutoff_seq);

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

    /// Create a closed genesis ledger holding only the canonical XRPL
    /// master AccountRoot — matches rippled's bootstrap when
    /// `genesis_amendments_disabled = true` (the xrpl-confluence
    /// topology). Use this for networked nodes; the
    /// `genesis_with_funded_account*` variants additionally pre-activate
    /// the 28 standalone amendments and are intended for solo/test
    /// scenarios where peers run the same bootstrap.
    pub fn genesis_with_master_account_only(genesis_address: &str) -> Result<Ledger, NodeError> {
        let mut genesis = Ledger::genesis();

        let account_id = decode_account_id(genesis_address)
            .map_err(|e| NodeError::Config(format!("invalid genesis address: {e}")))?;
        let key = keylet::account(&account_id);

        // Same field set as the funded variant — PreviousTxnID + Seq are
        // emitted by rippled even at genesis and must be present in our
        // SLE bytes or the leaf hashes diverge.
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

        genesis.close(0, 0)?;
        Ok(genesis)
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
        // Amendments). Without FeeSettings, rxrpl's genesis hash diverges.
        Self::insert_genesis_fee_settings(&mut genesis)?;

        // Amendments SLE: rippled-2.6.2 pre-activates 28 amendments in
        // genesis. Without this SLE rxrpl's account_hash at seq=1 diverges
        // from rippled and mixed-validator consensus can't converge on any
        // round (every ProposeSet carries a different prev_ledger). Captured
        // empirically from a standalone rippled-2.6.2 (issue #76).
        Self::insert_genesis_amendments(&mut genesis)?;

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

        // Pre-activated amendments (rippled-2.6.2 compat)
        Self::insert_genesis_amendments(&mut genesis)?;

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

    fn insert_genesis_amendments(genesis: &mut Ledger) -> Result<(), NodeError> {
        let amendments_value = serde_json::json!({
            "LedgerEntryType": "Amendments",
            "Amendments": rxrpl_amendment::presets::rippled_2_6_2::GENESIS_AMENDMENTS_HEX,
            "Flags": 0,
        });
        let amendments_key = keylet::amendments();
        let json_bytes =
            serde_json::to_vec(&amendments_value).map_err(|e| NodeError::Config(e.to_string()))?;
        let data = rxrpl_ledger::sle_codec::encode_sle(&json_bytes)
            .map_err(|e| NodeError::Config(format!("failed to encode amendments: {e}")))?;
        genesis.put_state(amendments_key, data)?;
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
    #[allow(clippy::too_many_arguments)]
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
            tracing::debug!("flag ledger #{}: no amendment voting changes", ledger_seq);
            return rules;
        }

        for action in &actions {
            let tx = rxrpl_amendment::voting::make_enable_amendment_tx(action);
            match tx_engine.apply(&tx, ledger, &rules, fees) {
                Ok(result) => {
                    if result.is_success() {
                        match action {
                            rxrpl_amendment::AmendmentAction::GotMajority {
                                amendment_id, ..
                            } => {
                                tracing::info!(
                                    "amendment {} gained majority",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                            rxrpl_amendment::AmendmentAction::LostMajority { amendment_id } => {
                                tracing::info!(
                                    "amendment {} lost majority",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                            rxrpl_amendment::AmendmentAction::Activate { amendment_id } => {
                                tracing::info!(
                                    "amendment {} activated",
                                    hex::encode(amendment_id.as_bytes())
                                );
                            }
                        }
                    } else {
                        tracing::warn!("amendment pseudo-tx failed: {}", result);
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

    /// Forward a received `Validation` message into the consensus engine's
    /// negative-UNL tracker.
    ///
    /// This is the single plumbing point connecting the overlay validation
    /// stream (produced by `PeerManager` and surfaced via
    /// `ConsensusMessage::Validation`) to the engine's
    /// [`ConsensusEngine::record_validation`]. Only validators that have been
    /// previously registered via `register_validators` accumulate counts; all
    /// other ids are silently ignored by the tracker.
    pub fn record_validation_into_engine<A: rxrpl_consensus::ConsensusAdapter>(
        consensus: &mut ConsensusEngine<A>,
        validation: &rxrpl_consensus::types::Validation,
    ) {
        consensus.record_validation(validation.node_id);
    }

    /// Apply negative-UNL pseudo-transactions on a flag ledger.
    ///
    /// On flag ledgers (sequence % 256 == 0), the consensus engine's
    /// negative-UNL tracker is evaluated to produce zero or more
    /// `UNLModify` pseudo-transactions (disable / re-enable). Each
    /// generated pseudo-tx is applied to `ledger` via `tx_engine`, which
    /// mutates the `NegativeUNL` ledger entry.
    ///
    /// Mirrors [`Self::apply_amendment_voting`] for nUNL. Returns the
    /// result of each applied pseudo-tx (in emission order). Off a flag
    /// ledger, returns an empty vector and does not touch state.
    pub fn apply_negative_unl<A: rxrpl_consensus::ConsensusAdapter>(
        consensus: &mut ConsensusEngine<A>,
        ledger: &mut Ledger,
        tx_engine: &TxEngine,
        fees: &FeeSettings,
        ledger_seq: u32,
    ) -> Vec<TransactionResult> {
        let changes = consensus.evaluate_negative_unl(ledger_seq);
        if changes.is_empty() {
            return Vec::new();
        }

        let rules = Rules::new();
        let mut results = Vec::with_capacity(changes.len());
        for change in changes {
            let tx = serde_json::json!({
                "TransactionType": "UNLModify",
                "UNLModifyDisabling": if change.disable { 1u32 } else { 0u32 },
                "UNLModifyValidator": change.validator_key,
                "LedgerSequence": change.ledger_seq,
            });
            match tx_engine.apply(&tx, ledger, &rules, fees) {
                Ok(result) => {
                    if result.is_success() {
                        tracing::info!(
                            "nUNL pseudo-tx applied: {} validator {}",
                            if change.disable {
                                "disable"
                            } else {
                                "re-enable"
                            },
                            change.validator_key,
                        );
                    } else {
                        tracing::warn!("nUNL pseudo-tx failed: {}", result);
                    }
                    results.push(result);
                }
                Err(e) => {
                    tracing::error!("failed to apply nUNL pseudo-tx: {}", e);
                }
            }
        }
        results
    }

    /// Close the current ledger and return a new open ledger derived from it.
    ///
    /// Returns the closed ledger's hash and the new open ledger.
    ///
    /// The caller-supplied `close_time` is clamped via `eff_close_time` to
    /// ensure `close_time > parent_close_time`, mirroring rippled's
    /// `effCloseTime` invariant. Without this, two ledgers closed in the
    /// same resolution bucket carry equal `close_time` fields and produce
    /// divergent hashes from rippled.
    pub fn close_ledger(ledger: &mut Ledger, close_time: u32) -> Result<Hash256, NodeError> {
        let resolution = ledger.header.close_time_resolution as u32;
        let parent_close_time = ledger.header.parent_close_time;
        let eff = rxrpl_consensus::eff_close_time(close_time, resolution, parent_close_time);
        ledger.close(eff, 0)?;
        Ok(ledger.header.hash)
    }

    /// Build the consensus candidate transaction set from an open ledger.
    ///
    /// The open ledger's `tx_map` stores a JSON record per transaction; the
    /// consensus set needs the canonical binary so a peer can acquire and
    /// re-apply it. Each `tx_json` is re-serialized to canonical form. The
    /// set hash is the rippled-compatible SHAMap root either way (it is a
    /// function of the tx ids), so a transaction whose blob fails to encode
    /// still hashes correctly — it just cannot be served to a peer.
    fn collect_consensus_tx_set(ledger: &Ledger) -> TxSet {
        let mut items: Vec<(Hash256, Vec<u8>)> = Vec::new();
        ledger.tx_map.for_each(&mut |tx_hash, data| {
            let blob = serde_json::from_slice::<Value>(data)
                .ok()
                .and_then(|rec| rec.get("tx_json").cloned())
                .and_then(|tx_json| rxrpl_codec::binary::encode(&tx_json).ok())
                .unwrap_or_default();
            if blob.is_empty() {
                tracing::warn!("consensus tx-set: no canonical blob for tx {}", tx_hash);
            }
            items.push((*tx_hash, blob));
        });
        TxSet::from_items(items)
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

    /// Recursively collect every `NFTokenID` value (64-char hex) reachable in
    /// a JSON value — covers both tx_json fields (burn/offers) and metadata
    /// AffectedNodes (mint).
    fn collect_nftoken_ids(value: &Value, out: &mut std::collections::HashSet<String>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    if k == "NFTokenID" {
                        if let Some(s) = v.as_str() {
                            if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
                                out.insert(s.to_string());
                            }
                        }
                    }
                    Self::collect_nftoken_ids(v, out);
                }
            }
            Value::Array(items) => {
                for v in items {
                    Self::collect_nftoken_ids(v, out);
                }
            }
            _ => {}
        }
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

                // Index by NFTokenID: a tx may touch several NFTs (mint emits
                // the id in metadata; burn/offers carry it in tx_json), so
                // collect every NFTokenID appearing in the record and map each
                // to this tx.
                let mut nft_ids = std::collections::HashSet::new();
                Self::collect_nftoken_ids(&record, &mut nft_ids);
                for nft_id_hex in &nft_ids {
                    if let Ok(bytes) = hex::decode(nft_id_hex) {
                        if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
                            let nft_id = Hash256::new(arr);
                            if let Err(e) = store.insert_nft_transaction(
                                nft_id.as_bytes(),
                                seq,
                                tx_index,
                                tx_hash.as_bytes(),
                            ) {
                                tracing::error!("failed to index nft tx: {}", e);
                            }
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

        let hash_bytes =
            hex::decode(hash_str).map_err(|e| format!("invalid ledger hash hex: {e}"))?;
        if hash_bytes.len() != 32 {
            return Err(format!("ledger hash must be 32 bytes, got {}", hash_bytes.len()).into());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&hash_bytes);
        let hash = Hash256::new(arr);

        Ok((seq, hash))
    }

    /// Fetch the validated ledger header via the RPC `ledger` command. Provides
    /// the `account_hash` that the bulk state download is verified against, plus
    /// the close-time / drops fields needed to build a header the next local
    /// close agrees with.
    async fn fetch_validated_header(
        rpc_url: &str,
        ledger_hash: &str,
    ) -> Result<rxrpl_ledger::LedgerHeader, Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .danger_accept_invalid_certs(true)
            .build()?;
        let resp = client
            .post(rpc_url)
            .json(&serde_json::json!({
                "method": "ledger",
                "params": [{"ledger_hash": ledger_hash, "transactions": false, "expand": false}]
            }))
            .send()
            .await?
            .json::<serde_json::Value>()
            .await?;
        let l = resp
            .get("result")
            .and_then(|r| r.get("ledger"))
            .ok_or("missing result.ledger in ledger response")?;

        let hash32 = |v: Option<&serde_json::Value>| -> Result<Hash256, String> {
            let s = v.and_then(|x| x.as_str()).ok_or("missing hash field")?;
            let b = hex::decode(s).map_err(|e| format!("bad hex: {e}"))?;
            let arr: [u8; 32] = b.as_slice().try_into().map_err(|_| "hash not 32 bytes")?;
            Ok(Hash256::new(arr))
        };
        // Numeric fields can arrive as JSON numbers or strings depending on the
        // server; accept both.
        let num = |v: Option<&serde_json::Value>| -> u64 {
            v.and_then(|x| {
                x.as_u64()
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
        };

        let mut header = rxrpl_ledger::LedgerHeader::new();
        header.sequence = num(l.get("ledger_index")) as u32;
        header.drops = num(l.get("total_coins"));
        header.parent_hash = hash32(l.get("parent_hash"))?;
        header.tx_hash = hash32(l.get("transaction_hash"))?;
        header.account_hash = hash32(l.get("account_hash"))?;
        header.parent_close_time = num(l.get("parent_close_time")) as u32;
        header.close_time = num(l.get("close_time")) as u32;
        header.close_time_resolution = num(l.get("close_time_resolution")) as u8;
        header.close_flags = num(l.get("close_flags")) as u8;
        header.hash = hash32(l.get("ledger_hash"))?;
        Ok(header)
    }

    /// Bulk-acquire a ledger's full account state via the RPC `ledger_data`
    /// pagination, rebuild the SHAMap locally, and verify its root equals the
    /// validated `expected_account_hash`. This is the fast, completing
    /// alternative to node-by-node P2P SHAMap sync (which peers rate-limit and
    /// which races the moving tip): pages of ~2048 entries from one full-history
    /// server, built into the tree in O(n), proven against the trusted root.
    async fn download_state_via_rpc(
        rpc_url: &str,
        ledger_hash: &str,
        expected_account_hash: Hash256,
        store: Arc<dyn NodeStore>,
    ) -> Result<SHAMap, Box<dyn std::error::Error + Send + Sync>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .danger_accept_invalid_certs(true)
            .build()?;

        let mut state_map = SHAMap::account_state_with_store(Arc::clone(&store));
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

            let result = body
                .get("result")
                .ok_or("missing result in ledger_data response")?;

            if let Some(err) = result.get("error") {
                return Err(format!("ledger_data error: {}", err).into());
            }

            let state = result
                .get("state")
                .and_then(|s| s.as_array())
                .ok_or("missing state array in ledger_data response")?;

            if state.is_empty() {
                break;
            }

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

                // Insert as an account-state leaf; `put` computes the correct
                // leaf hash (SHA512Half(MLN\0 || data || key)) and builds the
                // inner nodes, so the rebuilt root can be checked against the
                // validated account_hash below.
                let key = Hash256::new(key_bytes.as_slice().try_into().unwrap());
                state_map.put(key, data_bytes)?;
                total += 1;
            }
            page += 1;

            if page % 100 == 0 {
                let elapsed = start.elapsed().as_secs();
                let rate = (total as u64).checked_div(elapsed).unwrap_or(0);
                tracing::info!(
                    "RPC state download: {} entries ({} pages, {} entries/s)",
                    total,
                    page,
                    rate
                );
            }

            marker = result
                .get("marker")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if marker.is_none() {
                break;
            }
        }

        state_map.flush()?;
        let root = state_map.root_hash();
        let elapsed = start.elapsed().as_secs();
        if root != expected_account_hash {
            return Err(format!(
                "RPC state verification FAILED: rebuilt root {} != validated account_hash {} \
                 ({} entries, {} pages, {}s)",
                root, expected_account_hash, total, page, elapsed
            )
            .into());
        }
        tracing::info!(
            "RPC state download VERIFIED: {} entries in {} pages ({}s), root == account_hash {}",
            total,
            page,
            elapsed,
            root
        );
        Ok(state_map)
    }
}

/// Build a [`ValidatorIdentity`] from a [`ValidatorIdentityConfig`].
///
/// Returns `Ok(None)` when no signing identity is configured (the node will
/// run as a non-signing observer). Returns `Ok(Some(_))` when an explicit
/// two-key form (`master_secret` + `ephemeral_seed`) is provided.
///
/// `validator_token` and `validator_token_path` paths are not yet wired —
/// see follow-up B-track tasks. They currently return an error to flag the
/// gap rather than silently dropping the configuration.
fn build_validator_identity(
    cfg: &rxrpl_config::ValidatorIdentityConfig,
) -> Result<Option<rxrpl_overlay::identity::ValidatorIdentity>, NodeError> {
    let any_set = cfg.master_secret.is_some()
        || cfg.ephemeral_seed.is_some()
        || cfg.validator_token.is_some()
        || cfg.validator_token_path.is_some();
    if !any_set {
        return Ok(None);
    }

    if cfg.validator_token.is_some() || cfg.validator_token_path.is_some() {
        let token_str = match (&cfg.validator_token, &cfg.validator_token_path) {
            (Some(t), _) => t.clone(),
            (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| {
                NodeError::Config(format!(
                    "failed to read validator_token_path {}: {e}",
                    path.display()
                ))
            })?,
            (None, None) => unreachable!("guarded by the is_some checks above"),
        };
        let token = rxrpl_config::parse_validator_token(&token_str)
            .map_err(|e| NodeError::Config(format!("invalid validator_token: {e}")))?;
        // The manifest is master-signed and binds the ephemeral signing key;
        // parse_and_verify checks both signatures, so a malformed or tampered
        // token is rejected here rather than producing untrusted validations.
        let manifest = rxrpl_overlay::manifest::parse_and_verify(&token.manifest)
            .map_err(|e| NodeError::Config(format!("validator_token manifest invalid: {e:?}")))?;
        let signing_pubkey = manifest.ephemeral_public_key.ok_or_else(|| {
            NodeError::Config("validator_token manifest is a revocation (no signing key)".into())
        })?;
        let signing = rxrpl_crypto::KeyPair {
            public_key: signing_pubkey,
            private_key: token.validation_secret_key,
            key_type: rxrpl_crypto::KeyType::Secp256k1,
        };
        return Ok(Some(
            rxrpl_overlay::identity::ValidatorIdentity::from_token(
                manifest.master_public_key,
                signing,
                manifest.raw,
            ),
        ));
    }

    let master_str = cfg.master_secret.as_deref().ok_or_else(|| {
        NodeError::Config(
            "validator_identity.master_secret is required when ephemeral_seed is set".into(),
        )
    })?;
    let ephemeral_str = cfg.ephemeral_seed.as_deref().ok_or_else(|| {
        NodeError::Config(
            "validator_identity.ephemeral_seed is required when master_secret is set".into(),
        )
    })?;

    let (master_bytes, _master_kt) = parse_node_seed_with_type(master_str)
        .map_err(|e| NodeError::Config(format!("invalid validator_identity.master_secret: {e}")))?;
    let (ephemeral_bytes, _ephemeral_kt) =
        parse_node_seed_with_type(ephemeral_str).map_err(|e| {
            NodeError::Config(format!("invalid validator_identity.ephemeral_seed: {e}"))
        })?;

    // Validator identities MUST be secp256k1, regardless of the seed
    // prefix. Rippled's `PeerImp::onMessage(TMProposeSet)` enforces
    // `publicKeyType(nodepubkey) == KeyType::secp256k1` and rejects any
    // proposal whose public key starts with `0xED` (ed25519). So even
    // when the operator's seed is `sEd...` (the ed25519 family-seed
    // prefix from XLS-1d / wallet_propose), the resulting validator
    // identity has to be secp256k1 or every cross-impl peer drops our
    // proposals as "Proposal: malformed". The seed entropy is still
    // honored — only the key algorithm is forced.
    let kt = rxrpl_crypto::KeyType::Secp256k1;
    let master_seed = rxrpl_crypto::Seed::from_bytes(master_bytes);
    let ephemeral_seed = rxrpl_crypto::Seed::from_bytes(ephemeral_bytes);
    Ok(Some(
        rxrpl_overlay::identity::ValidatorIdentity::two_key_typed(
            &master_seed,
            kt,
            &ephemeral_seed,
            kt,
        ),
    ))
}

/// Like `parse_node_seed` but also returns the inferred [`KeyType`] from
/// the base58 family-seed prefix (`sEd...` → ed25519, `sn...`/`sp...` →
/// secp256k1). Hex-only input defaults to secp256k1 since the raw bytes
/// carry no prefix.
fn parse_node_seed_with_type(s: &str) -> Result<([u8; 16], rxrpl_crypto::KeyType), String> {
    let trimmed = s.trim();
    if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        let bytes = hex::decode(trimmed).map_err(|e| format!("invalid hex: {e}"))?;
        if bytes.len() != 16 {
            return Err("hex seed must decode to 16 bytes".into());
        }
        let mut out = [0u8; 16];
        out.copy_from_slice(&bytes);
        return Ok((out, rxrpl_crypto::KeyType::Secp256k1));
    }
    rxrpl_codec::address::seed::decode_seed(trimmed)
        .map_err(|e| format!("invalid base58 family seed: {e}"))
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
mod tests;
