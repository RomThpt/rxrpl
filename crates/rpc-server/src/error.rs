/// Errors from RPC server operations.
#[derive(Debug, thiserror::Error)]
pub enum RpcServerError {
    #[error("method not found: {0}")]
    MethodNotFound(String),

    #[error("invalid params: {0}")]
    InvalidParams(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("no permission: {0}")]
    NoPermission(String),
}
