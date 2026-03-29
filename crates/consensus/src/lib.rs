/// XRPL Ripple Protocol Consensus Algorithm (RPCA).
///
/// Provides:
/// - `ConsensusEngine`: State machine implementing RPCA
/// - `ConsensusAdapter`: Pluggable I/O interface
/// - `ConsensusPhase`: Open/Establish/Accepted phases
/// - Consensus types: `Proposal`, `Validation`, `TxSet`, `DisputedTx`
/// - `TrustedValidatorList`: UNL management
pub mod adapter;
pub mod engine;
pub mod error;
pub mod params;
pub mod phase;
pub mod simulator;
pub mod timer;
pub mod types;
pub mod unl;

pub use adapter::ConsensusAdapter;
pub use engine::{ConsensusEngine, WrongPrevLedgerDetected};
pub use error::ConsensusError;
pub use params::ConsensusParams;
pub use phase::ConsensusPhase;
pub use timer::{ConsensusTimer, TimerAction};
pub use types::{DisputedTx, NodeId, Proposal, TxSet, Validation};
pub use unl::TrustedValidatorList;
