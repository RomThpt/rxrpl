use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("invalid base58 character")]
    InvalidBase58,
    #[error("invalid checksum")]
    InvalidChecksum,
    #[error("invalid length: expected {expected}, got {got}")]
    InvalidLength { expected: usize, got: usize },
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("invalid seed: {0}")]
    InvalidSeed(String),
    #[error("unknown field: {0}")]
    UnknownField(String),
    #[error("invalid field id")]
    InvalidFieldId,
    #[error("unexpected end of data")]
    UnexpectedEnd,
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("hex error: {0}")]
    Hex(String),
    #[error("unsupported type: {0}")]
    UnsupportedType(String),
}
