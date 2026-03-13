/// Require destination tags for incoming payments.
pub const TF_REQUIRE_DEST_TAG: u32 = 0x0001_0000;

/// Set the RequireAuth flag (require authorization to hold IOUs).
pub const ASF_REQUIRE_AUTH: u32 = 2;

/// Set the DisallowXRP flag.
pub const ASF_DISALLOW_XRP: u32 = 3;

/// Set the RequireDest flag.
pub const ASF_REQUIRE_DEST: u32 = 1;

/// Disable the master key pair.
pub const ASF_DISABLE_MASTER: u32 = 4;

/// Enable no-freeze for this account's issued tokens.
pub const ASF_NO_FREEZE: u32 = 6;

/// Enable global freeze for this account's issued tokens.
pub const ASF_GLOBAL_FREEZE: u32 = 7;

/// Enable deposit authorization.
pub const ASF_DEPOSIT_AUTH: u32 = 9;

/// Enable AccountTxnID tracking.
pub const ASF_ACCOUNT_TXN_ID: u32 = 5;

/// Authorized minting of NFTokens.
pub const ASF_AUTHORIZED_NFTOKEN_MINTER: u32 = 10;

/// Default ripple flag for trust lines.
pub const ASF_DEFAULT_RIPPLE: u32 = 8;

/// Allow trustline clawback.
pub const ASF_ALLOW_TRUSTLINE_CLAWBACK: u32 = 16;
