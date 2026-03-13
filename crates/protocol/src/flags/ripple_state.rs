//! Ledger entry flags for RippleState (trust line) objects.

/// Low account has authorized the high account to hold its IOUs.
pub const LSF_LOW_AUTH: u32 = 0x0001_0000;

/// High account has authorized the low account to hold its IOUs.
pub const LSF_HIGH_AUTH: u32 = 0x0002_0000;

/// Low account has set No Ripple on this trust line.
pub const LSF_LOW_NO_RIPPLE: u32 = 0x0004_0000;

/// High account has set No Ripple on this trust line.
pub const LSF_HIGH_NO_RIPPLE: u32 = 0x0008_0000;

/// Low account has frozen this trust line.
pub const LSF_LOW_FREEZE: u32 = 0x0010_0000;

/// High account has frozen this trust line.
pub const LSF_HIGH_FREEZE: u32 = 0x0020_0000;
