/// Errors from node operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Storage(#[from] rxrpl_storage::StorageError),

    #[error("server error: {0}")]
    Server(String),

    #[error("already running")]
    AlreadyRunning,

    #[error("not running")]
    NotRunning,

    #[error("ledger error: {0}")]
    Ledger(#[from] rxrpl_ledger::LedgerError),

    #[error("transaction engine error: {0}")]
    TxEngine(#[from] rxrpl_tx_engine::TxEngineError),

    #[error("ledger not open")]
    LedgerNotOpen,

    #[error("validator seed file error: {0}")]
    SeedFile(String),
}
