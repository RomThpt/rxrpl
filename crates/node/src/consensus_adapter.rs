use rxrpl_consensus::ConsensusAdapter;
use rxrpl_consensus::types::{Proposal, TxSet, Validation};
use rxrpl_primitives::Hash256;

pub const MAX_CLOSED_LEDGERS: usize = 256;

/// Consensus adapter for standalone node operation.
///
/// All network and ledger operations are no-ops. The node's close loop
/// handles ledger lifecycle directly using consensus results.
#[derive(Default)]
pub struct NodeConsensusAdapter;

impl NodeConsensusAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl ConsensusAdapter for NodeConsensusAdapter {
    fn propose(&self, _proposal: &Proposal) {}

    fn share_position(&self, _proposal: &Proposal) {}

    fn share_tx(&self, _tx_hash: &Hash256, _tx_data: &[u8]) {}

    fn acquire_tx_set(&self, _hash: &Hash256) -> Option<TxSet> {
        None
    }

    fn on_close(&self, _: &Hash256, _: u32, _: u32, _: &TxSet) {}

    fn on_accept(&self, _validation: &Validation) {}

    fn on_accept_ledger(&self, _tx_set: &TxSet, _close_time: u32, _close_flags: u8) -> Hash256 {
        // In standalone mode, the close loop handles ledger lifecycle directly.
        // Return ZERO as a sentinel; the actual hash is computed by the close loop.
        Hash256::ZERO
    }
}
