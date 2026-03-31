//! Hook execution context.

use std::collections::HashMap;

use rxrpl_primitives::Hash256;

/// Maximum gas units a hook may consume per execution.
pub const DEFAULT_GAS_LIMIT: u64 = 1_000_000;

/// Maximum number of slots available for ledger entry data.
pub const MAX_SLOTS: usize = 16;

/// Maximum number of transactions a hook may emit.
pub const MAX_EMITTED_TXNS: usize = 256;

/// Execution context passed to a running hook.
///
/// Contains the originating transaction hash, the account that owns the hook,
/// a key-value state store, originating transaction data, slot storage for
/// ledger entries, emitted transactions, and a gas budget.
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
    /// Full serialized originating transaction blob.
    pub otxn_blob: Vec<u8>,
    /// Transaction type code of the originating transaction.
    pub otxn_type: u16,
    /// Account ID (20 bytes) from the originating transaction.
    pub otxn_account: [u8; 20],
    /// Amount in drops (for XRP payments) or 0.
    pub otxn_amount: i64,
    /// Originating transaction fields: field ID -> serialized value.
    pub otxn_fields: HashMap<u32, Vec<u8>>,
    /// Slot storage for ledger entries (up to 16 slots).
    pub slot_data: Vec<Option<Vec<u8>>>,
    /// Transactions emitted by this hook execution.
    pub emitted_txns: Vec<Vec<u8>>,
}

impl HookContext {
    /// Create a new context with the default gas limit.
    pub fn new(tx_hash: Hash256, account: [u8; 20]) -> Self {
        Self {
            tx_hash,
            account,
            state: HashMap::new(),
            gas_remaining: DEFAULT_GAS_LIMIT,
            otxn_blob: Vec::new(),
            otxn_type: 0,
            otxn_account: [0u8; 20],
            otxn_amount: 0,
            otxn_fields: HashMap::new(),
            slot_data: vec![None; MAX_SLOTS],
            emitted_txns: Vec::new(),
        }
    }

    /// Create a new context with a custom gas limit.
    pub fn with_gas(tx_hash: Hash256, account: [u8; 20], gas_limit: u64) -> Self {
        Self {
            tx_hash,
            account,
            state: HashMap::new(),
            gas_remaining: gas_limit,
            otxn_blob: Vec::new(),
            otxn_type: 0,
            otxn_account: [0u8; 20],
            otxn_amount: 0,
            otxn_fields: HashMap::new(),
            slot_data: vec![None; MAX_SLOTS],
            emitted_txns: Vec::new(),
        }
    }

    /// Create a context populated with originating transaction data.
    pub fn with_otxn(
        tx_hash: Hash256,
        account: [u8; 20],
        otxn_blob: Vec<u8>,
        otxn_type: u16,
        otxn_account: [u8; 20],
        otxn_amount: i64,
    ) -> Self {
        Self {
            tx_hash,
            account,
            state: HashMap::new(),
            gas_remaining: DEFAULT_GAS_LIMIT,
            otxn_blob,
            otxn_type,
            otxn_account,
            otxn_amount,
            otxn_fields: HashMap::new(),
            slot_data: vec![None; MAX_SLOTS],
            emitted_txns: Vec::new(),
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

    #[test]
    fn new_context_has_empty_otxn_fields() {
        let ctx = HookContext::new(Hash256::default(), [0u8; 20]);
        assert!(ctx.otxn_blob.is_empty());
        assert_eq!(ctx.otxn_type, 0);
        assert_eq!(ctx.otxn_account, [0u8; 20]);
        assert_eq!(ctx.otxn_amount, 0);
        assert!(ctx.otxn_fields.is_empty());
        assert_eq!(ctx.slot_data.len(), MAX_SLOTS);
        assert!(ctx.slot_data.iter().all(|s| s.is_none()));
        assert!(ctx.emitted_txns.is_empty());
    }

    #[test]
    fn with_otxn_sets_fields() {
        let tx_hash = Hash256::default();
        let account = [1u8; 20];
        let otxn_account = [2u8; 20];
        let blob = vec![0xAA, 0xBB];
        let ctx = HookContext::with_otxn(
            tx_hash,
            account,
            blob.clone(),
            1, // Payment
            otxn_account,
            1_000_000,
        );
        assert_eq!(ctx.otxn_blob, blob);
        assert_eq!(ctx.otxn_type, 1);
        assert_eq!(ctx.otxn_account, otxn_account);
        assert_eq!(ctx.otxn_amount, 1_000_000);
        assert_eq!(ctx.gas_remaining, DEFAULT_GAS_LIMIT);
        assert_eq!(ctx.slot_data.len(), MAX_SLOTS);
    }

    #[test]
    fn slot_data_initialized_to_none() {
        let ctx = HookContext::with_gas(Hash256::default(), [0u8; 20], 500);
        for slot in &ctx.slot_data {
            assert!(slot.is_none());
        }
    }
}
