use rxrpl_primitives::Hash256;

/// Errors from SHAMap operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SHAMapError {
    #[error("map is immutable")]
    Immutable,

    #[error("key not found")]
    NotFound,

    #[error("duplicate key")]
    DuplicateKey,

    #[error("invalid node data")]
    InvalidNode,

    #[error("invalid key length: expected 32, got {0}")]
    InvalidKeyLength(usize),

    #[error("node not found in store: {0}")]
    NodeNotFound(Hash256),

    #[error("no backing store for lazy loading")]
    MissingStore,

    #[error("failed to deserialize node")]
    DeserializeError,
}
