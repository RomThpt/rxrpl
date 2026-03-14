/// XRPL transaction execution engine.
///
/// Provides the full transaction processing pipeline:
/// - `Transactor` trait for transaction type handlers
/// - `TransactorRegistry` for type dispatch
/// - `TxEngine` for orchestrating the apply pipeline
/// - `ReadView`/`ApplyView`/`Sandbox` for COW state management
/// - Invariant checks for post-transaction validation
pub mod engine;
pub mod error;
pub mod fees;
pub mod handlers;
pub mod helpers;
pub mod invariants;
pub mod metadata;
pub mod registry;
pub mod transactor;
pub mod view;

pub use engine::TxEngine;
pub use error::TxEngineError;
pub use fees::FeeSettings;
pub use registry::TransactorRegistry;
pub use transactor::Transactor;
