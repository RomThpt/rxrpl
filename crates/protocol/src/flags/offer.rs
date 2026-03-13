//! Ledger entry flags for Offer objects.

/// This offer was placed as a passive offer.
pub const LSF_PASSIVE: u32 = 0x0001_0000;

/// This offer is a sell offer.
pub const LSF_SELL: u32 = 0x0002_0000;
