use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;

/// Provides access to closed ledgers for the P2P layer.
///
/// Used by PeerManager to serve GetLedger requests from peers.
pub trait LedgerProvider: Send + Sync + 'static {
    fn get_by_hash(&self, hash: &Hash256) -> Option<Ledger>;
    fn get_by_seq(&self, seq: u32) -> Option<Ledger>;
    fn latest_closed(&self) -> Option<Ledger>;

    /// Lowest sequence for which FetchPack data is still available.
    ///
    /// Providers without pruning metadata return `None`, preserving the
    /// historical behavior of serving every closed ledger they can resolve.
    fn earliest_fetch(&self) -> Option<u32> {
        None
    }
}
