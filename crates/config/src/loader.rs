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

    #[test]
    fn validator_identity_defaults_are_all_none() {
        let config = load_config_str("").unwrap();
        assert!(config.validator_identity.master_secret.is_none());
        assert!(config.validator_identity.ephemeral_seed.is_none());
        assert!(config.validator_identity.validator_token.is_none());
        assert!(config.validator_identity.validator_token_path.is_none());
        assert!(config.validator_identity.domain.is_none());
        assert_eq!(config.validator_identity.sequence, 0);
    }

    #[test]
    fn validator_identity_loads_explicit_two_key_form() {
        let toml = r#"
            [validator_identity]
            master_secret = "snoPBrXtMeMyMHUVTgbuqAfg1SUTb"
            ephemeral_seed = "shKqkZcvNqLLHvw1XBmRb1tYMu1z2"
            domain = "validator.example.com"
            sequence = 7
        "#;

        let config = load_config_str(toml).unwrap();
        let id = &config.validator_identity;
        assert_eq!(id.master_secret.as_deref(), Some("snoPBrXtMeMyMHUVTgbuqAfg1SUTb"));
        assert_eq!(id.ephemeral_seed.as_deref(), Some("shKqkZcvNqLLHvw1XBmRb1tYMu1z2"));
        assert_eq!(id.domain.as_deref(), Some("validator.example.com"));
        assert_eq!(id.sequence, 7);
        assert!(id.validator_token.is_none());
        assert!(id.validator_token_path.is_none());
    }
}
