use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;

/// Provides access to closed ledgers for the P2P layer.
///
/// Used by PeerManager to serve GetLedger requests from peers.
pub trait LedgerProvider: Send + Sync + 'static {
    fn get_by_hash(&self, hash: &Hash256) -> Option<Ledger>;
    fn get_by_seq(&self, seq: u32) -> Option<Ledger>;
    fn latest_closed(&self) -> Option<Ledger>;
}
