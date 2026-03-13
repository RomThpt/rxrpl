/// XRPL ledger state management.
///
/// Provides the core ledger lifecycle: genesis creation, open/closed/validated
/// state transitions, ledger header hashing, and state/transaction map management.
pub mod error;
pub mod header;
pub mod ledger;

pub use error::LedgerError;
pub use header::LedgerHeader;
pub use ledger::{Ledger, LedgerState};
