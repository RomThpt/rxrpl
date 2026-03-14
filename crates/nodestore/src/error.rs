use rxrpl_storage::StorageError;

/// Errors from node store operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeStoreError {
    #[error("node not found: {0}")]
    NotFound(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("encoding error: {0}")]
    Encoding(String),
}
