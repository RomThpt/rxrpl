//! Hook execution engine.

use std::sync::{Arc, Mutex};

use wasmi::{Engine, Module, Store};

use crate::context::HookContext;
use crate::hook_on;
use crate::host::{HostState, SlotLedger, register_host_functions};
use crate::validation::{self, ValidationError};

/// Result of hook execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookResult {
    /// Hook accepted the transaction with a result code.
    Accept(i64),
    /// Hook rolled back (negative result from the hook function).
    Rollback(i64),
    /// Hook execution failed due to an error.
    Error(String),
}

/// Outcome of executing one hook: its result plus any transactions it emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookExecution {
    pub result: HookResult,
    pub emitted_txns: Vec<Vec<u8>>,
}

/// Errors from the hook execution engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("wasm validation failed: {0}")]
    Validation(#[from] ValidationError),

    #[error("wasm instantiation failed: {0}")]
    Instantiation(String),

    #[error("hook execution failed: {0}")]
    Execution(String),
}

/// Loads, instantiates, and executes WASM hook modules.
pub struct HookExecutionEngine {
    engine: Engine,
}

impl Default for HookExecutionEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl HookExecutionEngine {
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    /// Execute a WASM hook binary with the given context.
    ///
    /// The `hook` export is called with a single i32 argument (0).
    /// Its i64 return value determines the result:
    /// - >= 0: Accept
    /// - < 0: Rollback
    pub fn execute(&self, wasm: &[u8], context: HookContext) -> Result<HookExecution, EngineError> {
        self.execute_with_ledger(wasm, context, None)
    }

    /// Execute a hook with an optional ledger for the `slot` host function to
    /// resolve keylets against.
    pub fn execute_with_ledger(
        &self,
        wasm: &[u8],
        context: HookContext,
        ledger: Option<&dyn SlotLedger>,
    ) -> Result<HookExecution, EngineError> {
        // Check HookOn filter before executing
        if let Some(ref hook_on_mask) = context.hook_on {
            if !hook_on::should_hook_fire(hook_on_mask, context.otxn_type) {
                return Ok(HookExecution {
                    result: HookResult::Accept(0),
                    emitted_txns: Vec::new(),
                });
            }
        }

        // Validate first
        validation::validate_wasm(wasm)?;

        let module = Module::new(&self.engine, wasm)
            .map_err(|e| EngineError::Instantiation(e.to_string()))?;

        let ctx = Arc::new(Mutex::new(context));
        let linker = register_host_functions(&self.engine, ctx.clone())
            .map_err(|e| EngineError::Instantiation(e.to_string()))?;

        let mut store = Store::new(&self.engine, HostState { ledger });

        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| EngineError::Instantiation(e.to_string()))?
            .start(&mut store)
            .map_err(|e| EngineError::Instantiation(e.to_string()))?;

        let hook_fn = instance
            .get_typed_func::<i32, i64>(&store, "hook")
            .map_err(|e| EngineError::Execution(e.to_string()))?;

        let result = match hook_fn.call(&mut store, 0) {
            Ok(code) if code >= 0 => HookResult::Accept(code),
            Ok(code) => HookResult::Rollback(code),
            Err(e) => HookResult::Error(e.to_string()),
        };

        let emitted_txns = std::mem::take(&mut ctx.lock().unwrap().emitted_txns);

        Ok(HookExecution {
            result,
            emitted_txns,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn accepting_hook_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
                (func $hook (export "hook") (param i32) (result i64)
                    i64.const 42
                )
            )
            "#,
        )
        .expect("valid WAT")
    }

    fn rejecting_hook_wasm() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
                (func $hook (export "hook") (param i32) (result i64)
                    i64.const -1
                )
            )
            "#,
        )
        .expect("valid WAT")
    }

    #[test]
    fn execute_accepting_hook() {
        let engine = HookExecutionEngine::new();
        let ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        let result = engine.execute(&accepting_hook_wasm(), ctx).unwrap().result;
        assert_eq!(result, HookResult::Accept(42));
    }

    #[test]
    fn execute_rejecting_hook() {
        let engine = HookExecutionEngine::new();
        let ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        let result = engine.execute(&rejecting_hook_wasm(), ctx).unwrap().result;
        assert_eq!(result, HookResult::Rollback(-1));
    }

    #[test]
    fn execute_invalid_wasm_returns_error() {
        let engine = HookExecutionEngine::new();
        let ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        let err = engine.execute(b"not wasm", ctx).unwrap_err();
        assert!(matches!(err, EngineError::Validation(_)));
    }

    #[test]
    fn execute_surfaces_emitted_txns() {
        let emitting_hook = wat::parse_str(
            r#"
            (module
                (import "env" "emit" (func $emit (param i32 i32 i32 i32) (result i64)))
                (memory (export "memory") 1)
                (data (i32.const 100) "\AA\BB\CC\DD\EE")
                (func $hook (export "hook") (param i32) (result i64)
                    i32.const 0
                    i32.const 32
                    i32.const 100
                    i32.const 5
                    call $emit
                    drop
                    i64.const 0
                )
            )
            "#,
        )
        .expect("valid WAT");
        let engine = HookExecutionEngine::new();
        let ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        let execution = engine.execute(&emitting_hook, ctx).unwrap();
        assert_eq!(execution.result, HookResult::Accept(0));
        assert_eq!(execution.emitted_txns, vec![vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE]]);
    }
}
