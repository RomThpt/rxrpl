/// The token may be burned by the issuer even if the issuer does not
/// currently hold the token.
pub const TF_BURNABLE: u32 = 0x0001;

/// Tokens can only be offered or sold for XRP.
pub const TF_ONLY_XRP: u32 = 0x0002;

/// Automatically create trust lines to hold transfer fees.
pub const TF_TRUSTLINE: u32 = 0x0004;

/// The token may be transferred to others.
pub const TF_TRANSFERABLE: u32 = 0x0008;

/// NFTokenMint: set to mint the token on behalf of another account.
pub const TF_NFTOKEN_MINT_ON_BEHALF: u32 = 0x0001_0000;

/// NFTokenCreateOffer: set if the offer is a sell offer.
pub const TF_SELL_NFTOKEN: u32 = 0x0000_0001;
