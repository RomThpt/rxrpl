use serde::{Deserialize, Serialize};

/// XRPL RPC error codes matching rippled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(i32)]
pub enum RpcErrorCode {
    Success = 0,
    BadSyntax = 1,
    JsonInvalid = 2,
    MissingCommand = 3,
    TooBusy = 4,
    NoNetwork = 5,
    NotReady = 6,
    NoClosed = 7,
    NoCurrent = 8,
    NotEnabled = 9,
    NotSupported = 10,
    LgrNotFound = 11,
    TxnNotFound = 12,
    LgrIdxMalformed = 13,
    ActNotFound = 14,
    ActMalformed = 15,
    UnknownCommand = 16,
    NoAccount = 17,
    InvalidParams = 18,
    SrcActNotFound = 19,
    SrcActMissing = 20,
    DstActMissing = 21,
    SrcCurMalformed = 22,
    SrcIsrMalformed = 23,
    DstAmtMalformed = 24,
    SrcAmtMalformed = 25,
    DstIsrMalformed = 26,
    InternalError = 27,
    SlowDown = 28,
    BadSecret = 29,
    BadSeed = 30,
    NotImpl = 31,
    BadFeature = 36,
    Forbidden = 37,
    NoEvents = 38,
    ChannelMalformed = 39,
    ChannelAmtMalformed = 40,
    ReportingUnsupported = 41,
}

impl RpcErrorCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::Success => "Success",
            Self::BadSyntax => "Bad syntax",
            Self::JsonInvalid => "Invalid JSON",
            Self::MissingCommand => "Missing command",
            Self::TooBusy => "Server too busy",
            Self::NoNetwork => "No network connection",
            Self::NotReady => "Not ready",
            Self::NoClosed => "No closed ledger",
            Self::NoCurrent => "No current ledger",
            Self::NotEnabled => "Feature not enabled",
            Self::NotSupported => "Not supported",
            Self::LgrNotFound => "Ledger not found",
            Self::TxnNotFound => "Transaction not found",
            Self::LgrIdxMalformed => "Ledger index malformed",
            Self::ActNotFound => "Account not found",
            Self::ActMalformed => "Account malformed",
            Self::UnknownCommand => "Unknown command",
            Self::NoAccount => "No account",
            Self::InvalidParams => "Invalid parameters",
            Self::SrcActNotFound => "Source account not found",
            Self::SrcActMissing => "Source account missing",
            Self::DstActMissing => "Destination account missing",
            Self::SrcCurMalformed => "Source currency malformed",
            Self::SrcIsrMalformed => "Source issuer malformed",
            Self::DstAmtMalformed => "Destination amount malformed",
            Self::SrcAmtMalformed => "Source amount malformed",
            Self::DstIsrMalformed => "Destination issuer malformed",
            Self::InternalError => "Internal error",
            Self::SlowDown => "Slow down",
            Self::BadSecret => "Bad secret",
            Self::BadSeed => "Bad seed",
            Self::NotImpl => "Not implemented",
            Self::BadFeature => "Bad feature",
            Self::Forbidden => "Forbidden",
            Self::NoEvents => "No events",
            Self::ChannelMalformed => "Channel malformed",
            Self::ChannelAmtMalformed => "Channel amount malformed",
            Self::ReportingUnsupported => "Reporting unsupported",
        }
    }
}
