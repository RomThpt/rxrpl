/// XRPL Ripple Protocol Consensus Algorithm (RPCA).
///
/// Provides:
/// - `ConsensusEngine`: State machine implementing RPCA
/// - `ConsensusAdapter`: Pluggable I/O interface
/// - `ConsensusPhase`: Open/Establish/Accepted phases
/// - Consensus types: `Proposal`, `Validation`, `TxSet`, `DisputedTx`
/// - `TrustedValidatorList`: UNL management
pub mod adapter;
pub mod close_resolution;
pub mod engine;
pub mod error;
pub mod negative_unl;
pub mod params;
pub mod phase;
pub mod simulator;
pub mod stall;
pub mod timer;
pub mod types;
pub mod unl;
pub mod validation_current;

pub use adapter::ConsensusAdapter;
pub use close_resolution::AdaptiveCloseTime;
pub use engine::{round_close_time, ConsensusEngine, WrongPrevLedgerDetected};
pub use error::ConsensusError;
pub use negative_unl::{NegativeUnlChange, NegativeUnlTracker, FLAG_LEDGER_INTERVAL};
pub use params::ConsensusParams;
pub use phase::ConsensusPhase;
pub use stall::{StallAction, StallMetrics};
pub use timer::{ConsensusTimer, TimerAction};
pub use types::{DisputedTx, NodeId, Proposal, TxSet, Validation};
pub use unl::TrustedValidatorList;
pub use validation_current::{
    is_current, VALIDATION_CURRENT_EARLY, VALIDATION_CURRENT_LOCAL, VALIDATION_CURRENT_WALL,
};
