use std::path::Path;

use crate::error::ConfigError;
use crate::types::NodeConfig;

/// Load node configuration from a TOML file.
///
/// Falls back to defaults for any missing fields.
pub fn load_config(path: impl AsRef<Path>) -> Result<NodeConfig, ConfigError> {
    let content = std::fs::read_to_string(path)?;
    let config: NodeConfig = toml::from_str(&content)?;
    Ok(config)
}

/// Load configuration from a TOML string.
pub fn load_config_str(toml_str: &str) -> Result<NodeConfig, ConfigError> {
    let config: NodeConfig = toml::from_str(toml_str)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_uses_defaults() {
        let config = load_config_str("").unwrap();
        assert_eq!(config.server.bind.port(), 5005);
        assert_eq!(config.peer.port, 51235);
        assert_eq!(config.peer.max_peers, 21);
    }

    #[test]
    fn partial_config() {
        let toml = r#"
            [server]
            bind = "0.0.0.0:8080"

            [peer]
            port = 9999
            seeds = ["peer1.example.com:51235"]

            [database]
            path = "/var/lib/rxrpl"
            backend = "rocksdb"

            [network]
            network_id = 1
        "#;

        let config = load_config_str(toml).unwrap();
        assert_eq!(config.server.bind.port(), 8080);
        assert_eq!(config.peer.port, 9999);
        assert_eq!(config.peer.seeds.len(), 1);
        assert_eq!(config.database.backend, "rocksdb");
        assert_eq!(config.network.network_id, 1);
    }

    #[test]
    fn validator_config_seed_file_field_parses() {
        let toml = r#"
            [validators]
            enabled = true
            seed_file = "/etc/rxrpl/validator-seed"
        "#;

        let config = load_config_str(toml).unwrap();
        assert!(config.validators.enabled);
        assert_eq!(
            config.validators.seed_file.as_deref(),
            Some(std::path::Path::new("/etc/rxrpl/validator-seed"))
        );
    }

    #[test]
    fn validator_config() {
        let toml = r#"
            [validators]
            enabled = true
            trusted = ["nHBtBkHGfL4NpB54H1AwBaaSJkSJLUSPvnUNAcuNpuffYB51VjH6"]
        "#;

        let config = load_config_str(toml).unwrap();
        assert!(config.validators.enabled);
        assert_eq!(config.validators.trusted.len(), 1);
    }
}
