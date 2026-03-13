use std::fmt;

use rxrpl_rpc_api::error::RpcErrorCode;

/// Client error type. All variants are `Clone`-friendly (non-Clone inner
/// errors are converted to `String` on construction via `From` impls).
#[derive(Clone, Debug)]
pub enum ClientError {
    Http(String),
    WebSocket(String),
    Json(String),
    Rpc {
        error: String,
        code: i32,
        message: Option<String>,
    },
    Connection(String),
    Timeout,
    SubscriptionClosed,
    InvalidUrl(String),
    Other(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::WebSocket(e) => write!(f, "WebSocket error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Rpc {
                error,
                code,
                message,
            } => {
                write!(f, "RPC error: {error} ({code})")?;
                if let Some(msg) = message {
                    write!(f, ": {msg}")?;
                }
                Ok(())
            }
            Self::Connection(e) => write!(f, "connection error: {e}"),
            Self::Timeout => write!(f, "timeout"),
            Self::SubscriptionClosed => write!(f, "subscription closed"),
            Self::InvalidUrl(e) => write!(f, "invalid URL: {e}"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<reqwest::Error> for ClientError {
    fn from(err: reqwest::Error) -> Self {
        Self::Http(err.to_string())
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err.to_string())
    }
}

impl ClientError {
    /// Returns `true` for errors that are likely transient and worth retrying.
    ///
    /// Transient errors include timeouts, connection failures, and specific RPC
    /// codes that indicate temporary server-side conditions (TooBusy, NoNetwork,
    /// NotReady, NoClosed, NoCurrent, SlowDown).
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Timeout | Self::Connection(_) => true,
            Self::Rpc { code, .. } => matches!(
                *code,
                4  // TooBusy
                | 5  // NoNetwork
                | 6  // NotReady
                | 7  // NoClosed
                | 8  // NoCurrent
                | 28 // SlowDown
            ),
            _ => false,
        }
    }

    /// Alias for [`is_transient`](Self::is_transient).
    pub fn is_retryable(&self) -> bool {
        self.is_transient()
    }

    /// If this is an `Rpc` error, attempt to map its code to a known
    /// [`RpcErrorCode`]. Returns `None` for non-Rpc variants or unknown codes.
    pub fn rpc_error_code(&self) -> Option<RpcErrorCode> {
        let code = match self {
            Self::Rpc { code, .. } => *code,
            _ => return None,
        };
        rpc_code_from_i32(code)
    }

    /// Returns `true` if this is an RPC error with the given code.
    pub fn is_rpc_error(&self, expected: RpcErrorCode) -> bool {
        self.rpc_error_code() == Some(expected)
    }
}

/// Map an `i32` to an `RpcErrorCode` variant. Returns `None` for unknown values.
fn rpc_code_from_i32(code: i32) -> Option<RpcErrorCode> {
    match code {
        0 => Some(RpcErrorCode::Success),
        1 => Some(RpcErrorCode::BadSyntax),
        2 => Some(RpcErrorCode::JsonInvalid),
        3 => Some(RpcErrorCode::MissingCommand),
        4 => Some(RpcErrorCode::TooBusy),
        5 => Some(RpcErrorCode::NoNetwork),
        6 => Some(RpcErrorCode::NotReady),
        7 => Some(RpcErrorCode::NoClosed),
        8 => Some(RpcErrorCode::NoCurrent),
        9 => Some(RpcErrorCode::NotEnabled),
        10 => Some(RpcErrorCode::NotSupported),
        11 => Some(RpcErrorCode::LgrNotFound),
        12 => Some(RpcErrorCode::TxnNotFound),
        13 => Some(RpcErrorCode::LgrIdxMalformed),
        14 => Some(RpcErrorCode::ActNotFound),
        15 => Some(RpcErrorCode::ActMalformed),
        16 => Some(RpcErrorCode::UnknownCommand),
        17 => Some(RpcErrorCode::NoAccount),
        18 => Some(RpcErrorCode::InvalidParams),
        19 => Some(RpcErrorCode::SrcActNotFound),
        20 => Some(RpcErrorCode::SrcActMissing),
        21 => Some(RpcErrorCode::DstActMissing),
        22 => Some(RpcErrorCode::SrcCurMalformed),
        23 => Some(RpcErrorCode::SrcIsrMalformed),
        24 => Some(RpcErrorCode::DstAmtMalformed),
        25 => Some(RpcErrorCode::SrcAmtMalformed),
        26 => Some(RpcErrorCode::DstIsrMalformed),
        27 => Some(RpcErrorCode::InternalError),
        28 => Some(RpcErrorCode::SlowDown),
        29 => Some(RpcErrorCode::BadSecret),
        30 => Some(RpcErrorCode::BadSeed),
        31 => Some(RpcErrorCode::NotImpl),
        36 => Some(RpcErrorCode::BadFeature),
        37 => Some(RpcErrorCode::Forbidden),
        38 => Some(RpcErrorCode::NoEvents),
        39 => Some(RpcErrorCode::ChannelMalformed),
        40 => Some(RpcErrorCode::ChannelAmtMalformed),
        41 => Some(RpcErrorCode::ReportingUnsupported),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_is_transient() {
        assert!(ClientError::Timeout.is_transient());
    }

    #[test]
    fn connection_is_transient() {
        assert!(ClientError::Connection("reset".into()).is_transient());
    }

    #[test]
    fn too_busy_rpc_is_transient() {
        let err = ClientError::Rpc {
            error: "tooBusy".into(),
            code: 4,
            message: None,
        };
        assert!(err.is_transient());
    }

    #[test]
    fn slow_down_rpc_is_transient() {
        let err = ClientError::Rpc {
            error: "slowDown".into(),
            code: 28,
            message: None,
        };
        assert!(err.is_transient());
    }

    #[test]
    fn act_not_found_is_permanent() {
        let err = ClientError::Rpc {
            error: "actNotFound".into(),
            code: 14,
            message: Some("Account not found.".into()),
        };
        assert!(!err.is_transient());
    }

    #[test]
    fn json_error_is_permanent() {
        let err = ClientError::Json("unexpected EOF".into());
        assert!(!err.is_transient());
    }

    #[test]
    fn invalid_url_is_permanent() {
        let err = ClientError::InvalidUrl("ftp://bad".into());
        assert!(!err.is_transient());
    }

    #[test]
    fn client_error_is_clone() {
        let err = ClientError::Timeout;
        let _ = err.clone();

        let err2 = ClientError::Rpc {
            error: "test".into(),
            code: 1,
            message: Some("msg".into()),
        };
        let cloned = err2.clone();
        assert!(matches!(cloned, ClientError::Rpc { .. }));
    }

    #[test]
    fn rpc_error_code_unknown() {
        let err = ClientError::Rpc {
            error: "custom".into(),
            code: 999,
            message: None,
        };
        assert_eq!(err.rpc_error_code(), None);
    }
}
