/// XRPL transaction queue / mempool.
///
/// Provides:
/// - `TxQueue`: Fee-ordered transaction queue with per-account tracking
/// - `FeeLevel`: Fee level calculation for ordering
/// - `FeeMetrics`: Fee escalation based on queue utilization
pub mod error;
pub mod fee;
pub mod queue;

pub use error::TxqError;
pub use fee::{FeeLevel, FeeMetrics, BASE_FEE_LEVEL, MAX_ACCOUNT_QUEUE_DEPTH};
pub use queue::{QueueEntry, TxQueue};
