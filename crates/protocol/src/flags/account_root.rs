//! Ledger entry flags for AccountRoot objects (lsf* prefix in rippled).

/// Require authorization for users to hold IOUs issued by this account.
pub const LSF_REQUIRE_AUTH: u32 = 0x0004_0000;

/// Require a destination tag for incoming payments.
pub const LSF_REQUIRE_DEST_TAG: u32 = 0x0002_0000;

/// Disallow incoming XRP.
pub const LSF_DISALLOW_XRP: u32 = 0x0008_0000;

/// Master key pair is disabled.
pub const LSF_DISABLE_MASTER: u32 = 0x0010_0000;

/// This account has used AccountTxnID.
pub const LSF_ACCOUNT_TXN_ID: u32 = 0x0000_0004;

/// No-freeze: this account cannot freeze trust lines.
pub const LSF_NO_FREEZE: u32 = 0x0020_0000;

/// Global freeze: all trust lines from this account are frozen.
pub const LSF_GLOBAL_FREEZE: u32 = 0x0040_0000;

/// Default ripple enabled for trust lines.
pub const LSF_DEFAULT_RIPPLE: u32 = 0x0080_0000;

/// Deposit authorization enabled.
pub const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;

/// Password-spent (used for regular key setting).
pub const LSF_PASSWORD_SPENT: u32 = 0x0001_0000;

/// Allow trustline clawback.
pub const LSF_ALLOW_TRUSTLINE_CLAWBACK: u32 = 0x8000_0000;
