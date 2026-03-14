/// Errors from transaction queue operations.
#[derive(Debug, thiserror::Error)]
pub enum TxqError {
    #[error("queue full")]
    QueueFull,

    #[error("fee too low: need at least {0} drops")]
    FeeTooLow(u64),

    #[error("duplicate transaction")]
    Duplicate,

    #[error("transaction expired")]
    Expired,

    #[error("engine error: {0}")]
    Engine(#[from] rxrpl_tx_engine::TxEngineError),
}
