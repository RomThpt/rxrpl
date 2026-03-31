//! XRPL WASM hooks runtime.
//!
//! Provides validation, host functions, and an execution engine for
//! WASM-based hooks on the XRP Ledger.

pub mod context;
pub mod engine;
pub mod host;
pub mod validation;

pub use context::{HookContext, DEFAULT_GAS_LIMIT, MAX_EMITTED_TXNS, MAX_SLOTS};
pub use engine::{EngineError, HookExecutionEngine, HookResult};
pub use validation::validate_wasm;
