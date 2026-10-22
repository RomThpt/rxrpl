use rxrpl_primitives::Hash256;

use crate::view::read_view::ReadView;

/// Mutable view into a ledger's state for applying transactions.
///
/// Extends `ReadView` with write operations. Used inside `Sandbox`
/// to track mutations that can be committed or rolled back.
pub trait ApplyView: ReadView {
    /// Insert a new state entry. Errors if already exists.
    fn insert(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), ApplyError>;

    /// Update an existing state entry. Errors if not found.
    fn update(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), ApplyError>;

    /// Delete a state entry. Errors if not found.
    fn erase(&mut self, key: &Hash256) -> Result<(), ApplyError>;

    /// Record destroyed XRP drops (transaction fees).
    fn destroy_drops(&mut self, drops: u64);

    /// Record the amount this transaction actually delivered (rippled's
    /// `ApplyContext::deliver`). A Payment sets it only when the delivered
    /// amount differs from the requested `Amount` (a partial or path-limited
    /// delivery), and the engine then writes `sfDeliveredAmount` into the
    /// metadata. Default is a no-op for views that do not track it.
    fn set_delivered_amount(&mut self, _amount: serde_json::Value) {}

    /// Whether the `SortedDirectories` amendment is active. Gates directory
    /// maintenance: when enabled, owner directories are kept sorted and entry
    /// removal preserves order; otherwise rippled's legacy append / swap-with-
    /// last behaviour applies (correct for replaying pre-amendment ledgers).
    fn sorted_directories(&self) -> bool {
        false
    }

    /// Whether the `fixPreviousTxnID` amendment is active. Gates directory-node
    /// threading: when enabled, freshly created `DirectoryNode` pages carry
    /// `sfPreviousTxnID` / `sfPreviousTxnLgrSeq` (filled by central stamping);
    /// before the amendment rippled left directory nodes unthreaded, so
    /// replaying pre-amendment ledgers must omit those fields. Defaults to
    /// `true` (modern behaviour) for views that do not set it.
    fn thread_directories(&self) -> bool {
        true
    }

    /// Snapshot the current mutation state so a speculative sub-flow can be
    /// rolled back (rippled's nested `ApplyView`/`Sandbox` checkpointing, used by
    /// `flow`'s multi-pass strand trials). The returned token is opaque; pass it
    /// back to [`ApplyView::rollback`] to restore the view to exactly this point.
    ///
    /// The default (for non-`Sandbox` views) returns an empty token and
    /// `rollback` is a no-op — only the copy-on-write `Sandbox` supports true
    /// speculative nesting.
    fn checkpoint(&self) -> ApplyCheckpoint {
        ApplyCheckpoint(None)
    }

    /// Restore the view to a previous [`ApplyView::checkpoint`]. Mutations made
    /// after the checkpoint are discarded.
    fn rollback(&mut self, _cp: ApplyCheckpoint) {}
}

/// Opaque snapshot of an [`ApplyView`]'s mutation state, taken by
/// [`ApplyView::checkpoint`] and consumed by [`ApplyView::rollback`]. Carries a
/// type-erased copy of the concrete view's internal change set (only `Sandbox`
/// produces a non-empty one).
pub struct ApplyCheckpoint(pub(crate) Option<Box<dyn std::any::Any>>);

/// Errors from apply view operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("entry already exists")]
    AlreadyExists,

    #[error("entry not found")]
    NotFound,
}
