use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("unknown transaction type: {0}")]
    UnknownTransactionType(u16),

    #[error("unknown transaction type name: {0}")]
    UnknownTransactionTypeName(String),

    #[error("unknown ledger entry type: {0}")]
    UnknownLedgerEntryType(u16),

    #[error("unknown ledger entry type name: {0}")]
    UnknownLedgerEntryTypeName(String),

    #[error("unknown result code: {0}")]
    UnknownResultCode(i32),

    #[error("unknown result code name: {0}")]
    UnknownResultCodeName(String),

    #[error("missing field: {0}")]
    MissingField(String),

    #[error("invalid field value: {0}")]
    InvalidFieldValue(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("signing error: {0}")]
    Signing(String),

    #[error("codec error: {0}")]
    Codec(#[from] rxrpl_codec::CodecError),

    #[error("crypto error: {0}")]
    Crypto(#[from] rxrpl_crypto::CryptoError),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
