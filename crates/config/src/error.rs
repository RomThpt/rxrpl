use std::path::PathBuf;

/// Errors from configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("invalid configuration: {0}")]
    Invalid(String),

    #[error("validator seed file not found: {0}")]
    SeedFileNotFound(PathBuf),

    #[error(
        "validator seed file {path} has insecure permissions: mode {mode:#o} (expected 0o600)"
    )]
    SeedFilePermissionDenied { path: PathBuf, mode: u32 },

    #[error("validator seed file {path} is unreadable: {reason}")]
    SeedFileUnreadable { path: PathBuf, reason: String },

    #[error("validator seed file {0} has invalid contents: {1}")]
    SeedFileInvalidContents(PathBuf, String),

    #[error(
        "validator seed file permission enforcement is not supported on this platform: {0}"
    )]
    SeedFilePlatformUnsupported(PathBuf),
}
