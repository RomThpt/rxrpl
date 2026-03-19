use rxrpl_primitives::Hash256;

use crate::types::{Proposal, TxSet, Validation};

/// Pluggable I/O interface for the consensus engine.
///
/// Separates consensus logic from networking, allowing the engine
/// to be tested with mock adapters.
pub trait ConsensusAdapter: Send + Sync {
    /// Share our proposal with the network.
    fn propose(&self, proposal: &Proposal);

    /// Share a position change with the network.
    fn share_position(&self, proposal: &Proposal);

    /// Share a transaction with peers.
    fn share_tx(&self, tx_hash: &Hash256, tx_data: &[u8]);

    /// Acquire a transaction set from the network.
    fn acquire_tx_set(&self, hash: &Hash256) -> Option<TxSet>;

    /// Called when the ledger is closed.
    fn on_close(&self, ledger_hash: &Hash256, ledger_seq: u32, close_time: u32, tx_set: &TxSet);

    /// Called when the ledger is accepted (validated).
    fn on_accept(&self, validation: &Validation);

    /// Apply the accepted transaction set to the ledger.
    ///
    /// The adapter closes the current ledger with the given close_time and flags,
    /// opens the next ledger, and returns the resulting ledger hash.
    fn on_accept_ledger(&self, tx_set: &TxSet, close_time: u32, close_flags: u8) -> Hash256;
}
