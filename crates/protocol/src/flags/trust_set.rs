/// Authorize the other party to hold currency issued by this account.
pub const TF_SET_AUTH: u32 = 0x0001_0000;

/// Enable the No Ripple flag on this trust line.
pub const TF_SET_NO_RIPPLE: u32 = 0x0002_0000;

/// Disable the No Ripple flag on this trust line.
pub const TF_CLEAR_NO_RIPPLE: u32 = 0x0004_0000;

/// Freeze the trust line.
pub const TF_SET_FREEZE: u32 = 0x0010_0000;

/// Unfreeze the trust line.
pub const TF_CLEAR_FREEZE: u32 = 0x0020_0000;
