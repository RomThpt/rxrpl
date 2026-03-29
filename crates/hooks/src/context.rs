//! Hook execution context.

use std::collections::HashMap;

use rxrpl_primitives::Hash256;

/// Maximum gas units a hook may consume per execution.
pub const DEFAULT_GAS_LIMIT: u64 = 1_000_000;

/// Execution context passed to a running hook.
///
/// Contains the originating transaction hash, the account that owns the hook,
/// a key-value state store, and a gas budget.
#[derive(Clone, Debug)]
pub struct HookContext {
    /// Hash of the originating transaction.
    pub tx_hash: Hash256,
    /// Account that owns this hook.
    pub account: [u8; 20],
    /// Hook state: namespace-prefixed key -> value.
    pub state: HashMap<Vec<u8>, Vec<u8>>,
    /// Remaining gas budget.
    pub gas_remaining: u64,
}

impl HookContext {
    /// Create a new context with the default gas limit.
    pub fn new(tx_hash: Hash256, account: [u8; 20]) -> Self {
        Self {
            tx_hash,
            account,
            state: HashMap::new(),
            gas_remaining: DEFAULT_GAS_LIMIT,
        }
    }

    /// Create a new context with a custom gas limit.
    pub fn with_gas(tx_hash: Hash256, account: [u8; 20], gas_limit: u64) -> Self {
        Self {
            tx_hash,
            account,
            state: HashMap::new(),
            gas_remaining: gas_limit,
        }
    }

    /// Consume gas, returning an error if the budget is exhausted.
    pub fn consume_gas(&mut self, amount: u64) -> Result<(), HookGasError> {
        if amount > self.gas_remaining {
            self.gas_remaining = 0;
            return Err(HookGasError::Exhausted);
        }
        self.gas_remaining -= amount;
        Ok(())
    }
}

/// Gas-related errors.
#[derive(Debug, thiserror::Error)]
pub enum HookGasError {
    #[error("hook gas budget exhausted")]
    Exhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consume_gas_within_budget() {
        let mut ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        assert!(ctx.consume_gas(100).is_ok());
        assert_eq!(ctx.gas_remaining, DEFAULT_GAS_LIMIT - 100);
    }

    #[test]
    fn consume_gas_exhausted() {
        let mut ctx = HookContext::with_gas(Hash256::default(), [0u8; 20], 50);
        let err = ctx.consume_gas(51).unwrap_err();
        assert!(matches!(err, HookGasError::Exhausted));
        assert_eq!(ctx.gas_remaining, 0);
    }
}
