/// Deposit only LP Tokens.
pub const TF_LP_TOKEN: u32 = 0x0001_0000;

/// Deposit a single asset.
pub const TF_SINGLE_ASSET: u32 = 0x0008_0000;

/// Deposit both assets.
pub const TF_TWO_ASSET: u32 = 0x0010_0000;

/// Deposit one asset and receive LP Tokens.
pub const TF_ONE_ASSET_LP_TOKEN: u32 = 0x0020_0000;

/// Deposit up to a specified limit of LP Tokens.
pub const TF_LIMIT_LP_TOKEN: u32 = 0x0040_0000;

/// Deposit both assets if the AMM pool is empty.
pub const TF_TWO_ASSET_IF_EMPTY: u32 = 0x0080_0000;
