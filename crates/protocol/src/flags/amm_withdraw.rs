/// Withdraw only LP Tokens.
pub const TF_LP_TOKEN: u32 = 0x0001_0000;

/// Withdraw all assets by returning all LP Tokens.
pub const TF_WITHDRAW_ALL: u32 = 0x0002_0000;

/// Withdraw all of one asset by returning all LP Tokens.
pub const TF_ONE_ASSET_WITHDRAW_ALL: u32 = 0x0004_0000;

/// Withdraw a single asset.
pub const TF_SINGLE_ASSET: u32 = 0x0008_0000;

/// Withdraw both assets.
pub const TF_TWO_ASSET: u32 = 0x0010_0000;

/// Withdraw one asset and return LP Tokens.
pub const TF_ONE_ASSET_LP_TOKEN: u32 = 0x0020_0000;

/// Withdraw up to a specified limit of LP Tokens.
pub const TF_LIMIT_LP_TOKEN: u32 = 0x0040_0000;
