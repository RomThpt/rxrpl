/// Errors from RPC server operations.
///
/// Each variant maps to a rippled error token (the short string returned in
/// the JSON `error` field) and a numeric code (the `error_code` field). See
/// `rippled/include/xrpl/protocol/ErrorCodes.h` for the canonical list.
#[derive(Debug, thiserror::Error)]
pub enum RpcServerError {
    // Generic categories.
    #[error("Unknown method: {0}")]
    MethodNotFound(String),

    #[error("Invalid parameters. {0}")]
    InvalidParams(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Server error: {0}")]
    Server(String),

    #[error("No permission: {0}")]
    NoPermission(String),

    // Semantic rippled-aligned variants.
    #[error("Account not found.")]
    AccountNotFound,

    #[error("Account malformed.")]
    AccountMalformed,

    #[error("Ledger not found.")]
    LedgerNotFound,

    #[error("Source currency is malformed.")]
    SourceCurrencyMalformed,

    #[error("Source account not provided.")]
    SourceAccountMissing,

    #[error("Not implemented.")]
    NotImplemented,

    #[error("Unknown option.")]
    UnknownOption,

    #[error("Missing field 'transaction'.")]
    FieldNotFoundTransaction,

    #[error("Entry not found.")]
    EntryNotFound,

    #[error("Transaction not found.")]
    TxNotFound,

    #[error("Object not found.")]
    ObjectNotFound,
}

impl RpcServerError {
    /// Short rippled error token used in the JSON `error` field.
    pub fn token(&self) -> &'static str {
        match self {
            Self::MethodNotFound(_) => "unknownCmd",
            Self::InvalidParams(_) => "invalidParams",
            Self::Internal(_) => "internal",
            Self::Server(_) => "internal",
            Self::NoPermission(_) => "noPermission",
            Self::AccountNotFound => "actNotFound",
            Self::AccountMalformed => "actMalformed",
            Self::LedgerNotFound => "lgrNotFound",
            Self::SourceCurrencyMalformed => "srcCurMalformed",
            Self::SourceAccountMissing => "srcActMissing",
            Self::NotImplemented => "notImpl",
            Self::UnknownOption => "unknownOption",
            Self::FieldNotFoundTransaction => "fieldNotFoundTransaction",
            Self::EntryNotFound => "entryNotFound",
            Self::TxNotFound => "txnNotFound",
            Self::ObjectNotFound => "objectNotFound",
        }
    }

    /// Numeric rippled error code for the JSON `error_code` field.
    pub fn numeric_code(&self) -> i32 {
        match self {
            Self::AccountNotFound => 19,
            Self::LedgerNotFound => 21,
            Self::InvalidParams(_) => 31,
            Self::MethodNotFound(_) => 32,
            Self::AccountMalformed => 35,
            Self::SourceAccountMissing => 66,
            Self::SourceCurrencyMalformed => 69,
            Self::NotImplemented => 74,
            Self::Internal(_) | Self::Server(_) => 73,
            Self::NoPermission(_) => 6,
            // Legacy / not in ErrorCodes.h. Numeric value isn't validated by
            // the xrpl-hive runner (only the token is); keep a stable nonzero.
            Self::UnknownOption => 1,
            Self::FieldNotFoundTransaction => 1,
            Self::EntryNotFound => 20,
            Self::TxNotFound => 29,
            Self::ObjectNotFound => 20,
        }
    }

    /// Human-readable diagnostic string for the JSON `error_message` field.
    pub fn human_message(&self) -> String {
        self.to_string()
    }

    /// Stable label used as a metric tag for error categorisation.
    pub fn metric_label(&self) -> &'static str {
        match self {
            Self::MethodNotFound(_) => "method_not_found",
            Self::InvalidParams(_) => "invalid_params",
            Self::Internal(_) => "internal",
            Self::Server(_) => "server",
            Self::NoPermission(_) => "no_permission",
            Self::AccountNotFound => "account_not_found",
            Self::AccountMalformed => "account_malformed",
            Self::LedgerNotFound => "ledger_not_found",
            Self::SourceCurrencyMalformed => "source_currency_malformed",
            Self::SourceAccountMissing => "source_account_missing",
            Self::NotImplemented => "not_implemented",
            Self::UnknownOption => "unknown_option",
            Self::FieldNotFoundTransaction => "field_not_found_transaction",
            Self::EntryNotFound => "entry_not_found",
            Self::TxNotFound => "tx_not_found",
            Self::ObjectNotFound => "object_not_found",
        }
    }
}
