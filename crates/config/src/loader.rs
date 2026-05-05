use std::path::Path;

use rxrpl_crypto::Seed;

use crate::error::ConfigError;
use crate::seed_loader::load_seed_file;
use crate::types::NodeConfig;

/// Load node configuration from a TOML file.
///
/// Falls back to defaults for any missing fields.
pub fn load_config(path: impl AsRef<Path>) -> Result<NodeConfig, ConfigError> {
    let content = std::fs::read_to_string(path)?;
    let config: NodeConfig = toml::from_str(&content)?;
    Ok(config)
}

/// Load node configuration and, if `validators.seed_file` is configured,
/// the validator signing seed from that file (with strict permission checks).
///
/// Returns `(config, seed)` where `seed` is `Some` only if a seed file path
/// was specified and successfully loaded. Permission/format errors propagate
/// as [`ConfigError`].
pub fn load_config_with_seed(
    path: impl AsRef<Path>,
) -> Result<(NodeConfig, Option<Seed>), ConfigError> {
    let config = load_config(path)?;
    let seed = match config.validators.seed_file.as_deref() {
        Some(p) => Some(load_seed_file(p)?),
        None => None,
    };
    Ok((config, seed))
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

    #[cfg(unix)]
    #[test]
    fn load_config_with_seed_returns_none_when_unset() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("rxrpl.cfg");
        let mut f = std::fs::File::create(&cfg_path).unwrap();
        f.write_all(b"[validators]\nenabled = true\n").unwrap();
        let (cfg, seed) = load_config_with_seed(&cfg_path).unwrap();
        assert!(cfg.validators.enabled);
        assert!(seed.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn load_config_with_seed_loads_when_path_set_and_mode_ok() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let seed_path = dir.path().join("seed");
        let mut sf = std::fs::File::create(&seed_path).unwrap();
        sf.write_all(b"00112233445566778899aabbccddeeff").unwrap();
        std::fs::set_permissions(&seed_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let cfg_path = dir.path().join("rxrpl.cfg");
        let mut f = std::fs::File::create(&cfg_path).unwrap();
        write!(
            f,
            "[validators]\nenabled = true\nseed_file = \"{}\"\n",
            seed_path.display()
        )
        .unwrap();

        let (_cfg, seed) = load_config_with_seed(&cfg_path).unwrap();
        let seed = seed.expect("seed should be loaded");
        assert_eq!(seed.as_bytes()[0], 0x00);
        assert_eq!(seed.as_bytes()[15], 0xff);
    }

    #[cfg(unix)]
    #[test]
    fn load_config_with_seed_rejects_loose_permissions() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let seed_path = dir.path().join("seed");
        let mut sf = std::fs::File::create(&seed_path).unwrap();
        sf.write_all(b"00112233445566778899aabbccddeeff").unwrap();
        std::fs::set_permissions(&seed_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let cfg_path = dir.path().join("rxrpl.cfg");
        let mut f = std::fs::File::create(&cfg_path).unwrap();
        write!(
            f,
            "[validators]\nseed_file = \"{}\"\n",
            seed_path.display()
        )
        .unwrap();

        let err = load_config_with_seed(&cfg_path).unwrap_err();
        assert!(matches!(err, ConfigError::SeedFilePermissionDenied { .. }));
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
