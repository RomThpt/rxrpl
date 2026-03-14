use rxrpl_protocol::TransactionResult;

/// Errors from transaction engine operations.
#[derive(Debug, thiserror::Error)]
pub enum TxEngineError {
    #[error("transaction failed: {0}")]
    TransactionFailed(TransactionResult),

    #[error("unknown transaction type: {0}")]
    UnknownTransactionType(String),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("ledger error: {0}")]
    Ledger(#[from] rxrpl_ledger::LedgerError),

    #[error("shamap error: {0}")]
    SHAMap(#[from] rxrpl_shamap::SHAMapError),

    #[error("invariant violated: {0}")]
    InvariantViolated(String),
}
