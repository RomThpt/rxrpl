/// Errors from overlay network operations.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("connection failed: {0}")]
    Connection(String),

    #[error("handshake failed: {0}")]
    Handshake(String),

    #[error("peer limit reached")]
    PeerLimitReached,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("codec error: {0}")]
    Codec(String),
}
