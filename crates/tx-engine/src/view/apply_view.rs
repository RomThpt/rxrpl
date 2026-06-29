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
}

/// Errors from apply view operations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ApplyError {
    #[error("entry already exists")]
    AlreadyExists,

    #[error("entry not found")]
    NotFound,
}
