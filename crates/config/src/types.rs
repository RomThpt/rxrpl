use std::net::SocketAddr;
use std::path::PathBuf;

use serde::Deserialize;

/// Top-level node configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub peer: PeerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub validators: ValidatorConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub genesis: GenesisConfig,
    #[serde(default)]
    pub cluster: ClusterConfig,
    #[serde(default)]
    pub reporting: ReportingConfig,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            peer: PeerConfig::default(),
            database: DatabaseConfig::default(),
            validators: ValidatorConfig::default(),
            network: NetworkConfig::default(),
            genesis: GenesisConfig::default(),
            cluster: ClusterConfig::default(),
            reporting: ReportingConfig::default(),
        }
    }
}

/// Reporting mode configuration.
///
/// When enabled, the node operates as a read-only reporting server that
/// receives validated ledgers from an upstream ETL source and serves
/// historical data via RPC. No consensus or P2P participation.
#[derive(Clone, Debug, Deserialize)]
pub struct ReportingConfig {
    /// Whether reporting mode is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// WebSocket URL of the upstream ETL source (e.g., a validating node).
    #[serde(default = "default_etl_source")]
    pub etl_source: String,
    /// URL to forward write requests (submit, etc.) to.
    #[serde(default = "default_forward_url")]
    pub forward_url: String,
}

fn default_etl_source() -> String {
    "ws://127.0.0.1:6006".into()
}

fn default_forward_url() -> String {
    "http://127.0.0.1:5005".into()
}

impl Default for ReportingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            etl_source: default_etl_source(),
            forward_url: default_forward_url(),
        }
    }
}

/// Server (RPC) configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct ServerConfig {
    /// HTTP/WS bind address.
    #[serde(default = "default_rpc_addr")]
    pub bind: SocketAddr,
    /// Admin IP addresses (for admin RPC methods).
    #[serde(default)]
    pub admin_ips: Vec<String>,
}

fn default_rpc_addr() -> SocketAddr {
    "127.0.0.1:5005".parse().unwrap()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_rpc_addr(),
            admin_ips: vec!["127.0.0.1".into()],
        }
    }
}

/// Peer-to-peer networking configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct PeerConfig {
    /// Peer protocol listen port.
    #[serde(default = "default_peer_port")]
    pub port: u16,
    /// Maximum number of peer connections.
    #[serde(default = "default_max_peers")]
    pub max_peers: usize,
    /// Bootstrap peer addresses.
    #[serde(default = "default_seeds")]
    pub seeds: Vec<String>,
    /// Fixed peers (always connect to these).
    #[serde(default)]
    pub fixed_peers: Vec<String>,
    /// Hex seed for deterministic node identity. If None, a random identity is generated.
    #[serde(default)]
    pub node_seed: Option<String>,
    /// Enable TLS for P2P connections (default: true).
    #[serde(default = "default_tls_enabled")]
    pub tls_enabled: bool,
}

fn default_tls_enabled() -> bool {
    true
}

fn default_peer_port() -> u16 {
    51235
}

fn default_max_peers() -> usize {
    21
}

fn default_seeds() -> Vec<String> {
    vec![
        "r.ripple.com:51235".into(),
        "s1.ripple.com:51235".into(),
        "s2.ripple.com:51235".into(),
        "s.altnet.rippletest.net:51235".into(),
    ]
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            port: default_peer_port(),
            max_peers: default_max_peers(),
            seeds: default_seeds(),
            fixed_peers: Vec::new(),
            node_seed: None,
            tls_enabled: true,
        }
    }
}

/// Database configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct DatabaseConfig {
    /// Data directory path.
    #[serde(default = "default_data_dir")]
    pub path: PathBuf,
    /// Node store backend ("rocksdb" or "memory").
    #[serde(default = "default_backend")]
    pub backend: String,
    /// Number of most-recent ledgers to retain. Older ledger data is pruned.
    /// Set to 0 to keep all history (no pruning). Default: 2000.
    #[serde(default = "default_online_delete")]
    pub online_delete: u32,
    /// When true, automatic pruning is disabled and deletion only happens
    /// when triggered via the `can_delete` RPC command. Default: false.
    #[serde(default)]
    pub advisory_delete: bool,
    /// Shard store configuration for ledger history sharding.
    #[serde(default)]
    pub shard: ShardConfig,
}

/// Shard store configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct ShardConfig {
    /// Whether the shard store is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Directory path for shard data.
    #[serde(default = "default_shard_path")]
    pub path: String,
    /// Maximum number of shards to store locally.
    #[serde(default = "default_max_shards")]
    pub max_shards: u32,
}

fn default_shard_path() -> String {
    "data/shards".into()
}

fn default_max_shards() -> u32 {
    10
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: default_shard_path(),
            max_shards: default_max_shards(),
        }
    }
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_backend() -> String {
    "memory".into()
}

fn default_online_delete() -> u32 {
    2000
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_data_dir(),
            backend: default_backend(),
            online_delete: default_online_delete(),
            advisory_delete: false,
            shard: ShardConfig::default(),
        }
    }
}

/// Validator configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct ValidatorConfig {
    /// Whether this node is a validator.
    #[serde(default)]
    pub enabled: bool,
    /// Trusted validator public keys.
    #[serde(default)]
    pub trusted: Vec<String>,
    /// Validator list sites (UNL providers).
    #[serde(default)]
    pub validator_list_sites: Vec<String>,
    /// Validator list public keys.
    #[serde(default)]
    pub validator_list_keys: Vec<String>,
    /// Validation quorum override. None = auto-compute from validator list size.
    #[serde(default)]
    pub quorum: Option<usize>,
    /// When true, the validation aggregator only counts validations whose
    /// signing key is in the trusted UNL. Default: true (mainnet behavior).
    /// Tests / isolated multi-node sims should set this to false because they
    /// run with no UNL configured.
    #[serde(default = "default_require_trusted")]
    pub require_trusted_validators: bool,
    /// Optional path to a file containing this validator's signing seed.
    /// On Unix the file MUST have mode 0o600 or loading is refused.
    #[serde(default)]
    pub seed_file: Option<PathBuf>,
}

fn default_require_trusted() -> bool {
    true
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trusted: Vec::new(),
            validator_list_sites: Vec::new(),
            validator_list_keys: Vec::new(),
            quorum: None,
            require_trusted_validators: default_require_trusted(),
            seed_file: None,
        }
    }
}

/// Network configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct NetworkConfig {
    /// Network ID (0 = mainnet, 1 = testnet, etc.).
    #[serde(default)]
    pub network_id: u32,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self { network_id: 0 }
    }
}

/// Cluster configuration.
///
/// Defines a set of trusted cluster peer nodes that share load-balancing
/// and fee information through TMCluster messages.
#[derive(Clone, Debug, Deserialize)]
pub struct ClusterConfig {
    /// Whether cluster mode is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Human-readable name for this cluster node.
    #[serde(default)]
    pub node_name: Option<String>,
    /// Public keys of trusted cluster members (hex-encoded).
    #[serde(default)]
    pub members: Vec<String>,
    /// Interval in seconds between cluster status broadcasts.
    #[serde(default = "default_cluster_broadcast_interval")]
    pub broadcast_interval_secs: u64,
}

fn default_cluster_broadcast_interval() -> u64 {
    5
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            node_name: None,
            members: Vec::new(),
            broadcast_interval_secs: default_cluster_broadcast_interval(),
        }
    }
}

/// Genesis ledger configuration.
#[derive(Clone, Debug, Deserialize)]
pub struct GenesisConfig {
    /// Genesis ledger hash (for network identification).
    #[serde(default)]
    pub ledger_hash: Option<String>,
}

impl Default for GenesisConfig {
    fn default() -> Self {
        Self { ledger_hash: None }
    }
}
