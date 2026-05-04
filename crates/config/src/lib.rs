/// XRPL node configuration (TOML-based).
///
/// Provides configuration types for all aspects of a validator node:
/// server, peer networking, database, validators, and network settings.
pub mod error;
pub mod loader;
pub mod types;
pub mod validator_token;

pub use error::ConfigError;
pub use loader::load_config;
pub use types::{
    ClusterConfig, DatabaseConfig, GenesisConfig, NetworkConfig, NodeConfig, PeerConfig,
    ServerConfig, ShardConfig, ValidatorConfig, ValidatorIdentityConfig,
};
pub use validator_token::{ValidatorToken, ValidatorTokenError, parse_validator_token};
