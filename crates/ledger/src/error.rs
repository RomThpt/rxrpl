/// Errors from ledger operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("ledger is immutable")]
    Immutable,

    #[error("ledger is not closed")]
    NotClosed,

    #[error("ledger is already closed")]
    AlreadyClosed,

    #[error("ledger entry not found")]
    NotFound,

    #[error("codec error: {0}")]
    Codec(String),

    #[error("shamap error: {0}")]
    SHAMap(#[from] rxrpl_shamap::SHAMapError),
}
