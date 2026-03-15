use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use rxrpl_amendment::{AmendmentTable, FeatureRegistry, Rules};
use rxrpl_codec::address::classic::decode_account_id;
use rxrpl_config::NodeConfig;
use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;
use rxrpl_protocol::{keylet, TransactionResult};
use rxrpl_rpc_server::ServerContext;
use rxrpl_tx_engine::{FeeSettings, TransactorRegistry, TxEngine};
use rxrpl_txq::TxQueue;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::error::NodeError;

const MAX_CLOSED_LEDGERS: usize = 256;

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
    running: bool,
}

impl Node {
    /// Create a new node from configuration.
    pub fn new(config: NodeConfig) -> Result<Self, NodeError> {
        // Initialize amendment registry
        let registry = FeatureRegistry::with_known_amendments();
        let amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4); // ~14 days at 4s/ledger

        // Initialize transaction engine with Phase A + B handlers
        let mut tx_registry = TransactorRegistry::new();
        rxrpl_tx_engine::handlers::register_phase_a(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_b(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c1(&mut tx_registry);
        let tx_engine = TxEngine::new_without_sig_check(tx_registry);

        // Initialize genesis ledger
        let ledger = Ledger::genesis();

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
            running: false,
        })
    }

    /// Create a standalone node with a funded genesis account.
    ///
    /// Creates genesis ledger, funds the account, closes genesis,
    /// and opens ledger #2 ready for transactions.
    pub fn new_standalone(
        config: NodeConfig,
        genesis_address: &str,
    ) -> Result<Self, NodeError> {
        let registry = FeatureRegistry::with_known_amendments();
        let amendment_table = AmendmentTable::new(&registry, 14 * 24 * 60 * 4);

        let mut tx_registry = TransactorRegistry::new();
        rxrpl_tx_engine::handlers::register_phase_a(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_b(&mut tx_registry);
        rxrpl_tx_engine::handlers::register_phase_c1(&mut tx_registry);
        let tx_engine = TxEngine::new_without_sig_check(tx_registry);

        let closed_genesis = Self::genesis_with_funded_account(genesis_address)?;
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
        );

        let app = rxrpl_rpc_server::build_router(ctx);
        let bind = self.config.server.bind;

        tracing::info!("starting standalone RPC server on {}", bind);

        let listener = tokio::net::TcpListener::bind(bind)
            .await
            .map_err(|e| NodeError::Server(e.to_string()))?;

        // Spawn RPC server
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::error!("RPC server error: {}", e);
            }
        });

        // Spawn ledger close loop
        let ledger = Arc::clone(&self.ledger);
        let closed_ledgers = Arc::clone(&self.closed_ledgers);
        let interval_duration = Duration::from_secs(close_interval_secs);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval_duration);
            // Skip the first immediate tick
            interval.tick().await;

            loop {
                interval.tick().await;

                let mut l = ledger.write().await;
                let close_time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as u32;

                if let Err(e) = l.close(close_time, 0) {
                    tracing::error!("failed to close ledger: {}", e);
                    continue;
                }

                let hash = l.header.hash;
                let seq = l.header.sequence;
                let closed = l.clone();

                // Open next ledger
                *l = Ledger::new_open(&closed);

                // Store in history
                let mut history = closed_ledgers.write().await;
                history.push_back(closed);
                while history.len() > MAX_CLOSED_LEDGERS {
                    history.pop_front();
                }

                tracing::info!(
                    "closed ledger #{} hash={}, opened #{}",
                    seq,
                    hash,
                    l.header.sequence
                );
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

    /// Create a genesis ledger with a single funded account holding all XRP.
    ///
    /// Closes the genesis ledger and opens ledger #2 ready for transactions.
    pub fn genesis_with_funded_account(
        genesis_address: &str,
    ) -> Result<Ledger, NodeError> {
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
        let data = serde_json::to_vec(&account)
            .map_err(|e| NodeError::Config(e.to_string()))?;
        genesis.put_state(key, data)?;

        // Close genesis ledger
        genesis.close(0, 0)?;

        Ok(genesis)
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
    pub fn close_ledger(
        ledger: &mut Ledger,
        close_time: u32,
    ) -> Result<Hash256, NodeError> {
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
        let account: Value = serde_json::from_slice(data).unwrap();
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
}
