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
}
