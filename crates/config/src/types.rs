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
    #[serde(default)]
    pub seeds: Vec<String>,
    /// Fixed peers (always connect to these).
    #[serde(default)]
    pub fixed_peers: Vec<String>,
}

fn default_peer_port() -> u16 {
    51235
}

fn default_max_peers() -> usize {
    21
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self {
            port: default_peer_port(),
            max_peers: default_max_peers(),
            seeds: Vec::new(),
            fixed_peers: Vec::new(),
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
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}

fn default_backend() -> String {
    "memory".into()
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_data_dir(),
            backend: default_backend(),
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
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            trusted: Vec::new(),
            validator_list_sites: Vec::new(),
            validator_list_keys: Vec::new(),
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
