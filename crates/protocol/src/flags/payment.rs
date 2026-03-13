/// No special flags.
pub const TF_NO_FLAGS: u32 = 0;

/// Do not use the default path; only use paths included in the Paths field.
pub const TF_NO_RIPPLE_DIRECT: u32 = 0x0001_0000;

/// If the specified Amount cannot be sent without spending more than SendMax,
/// reduce the received amount instead of failing outright.
pub const TF_PARTIAL_PAYMENT: u32 = 0x0002_0000;

/// Only take paths where all the conversions have an input:output ratio
/// that is equal or better than the ratio of Amount:SendMax.
pub const TF_LIMIT_QUALITY: u32 = 0x0004_0000;
