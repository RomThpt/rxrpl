/// XRPL validator node orchestrator.
///
/// Wires together all subsystems into a running node:
/// overlay + rpc-server + consensus + tx-engine + txq + ledger store.
pub mod consensus_adapter;
pub mod error;
pub mod node;

pub use error::NodeError;
pub use node::Node;
