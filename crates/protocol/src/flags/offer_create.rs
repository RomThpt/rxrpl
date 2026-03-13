/// This offer is passive: it does not consume offers that exactly match it.
pub const TF_PASSIVE: u32 = 0x0001_0000;

/// Treat the offer as an Immediate or Cancel order: if not fully filled on
/// placement, unfilled remainder is not placed in the order book.
pub const TF_IMMEDIATE_OR_CANCEL: u32 = 0x0002_0000;

/// Treat the offer as a Fill or Kill order: if not fully filled on placement,
/// the entire order is cancelled.
pub const TF_FILL_OR_KILL: u32 = 0x0004_0000;

/// Exchange the entire TakerGets amount, even if it means obtaining more
/// than TakerPays in exchange.
pub const TF_SELL: u32 = 0x0008_0000;
