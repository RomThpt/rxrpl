use std::collections::HashMap;

use rxrpl_protocol::TransactionType;

use crate::transactor::Transactor;

/// Registry mapping transaction types to their handlers.
pub struct TransactorRegistry {
    handlers: HashMap<TransactionType, Box<dyn Transactor>>,
}

impl TransactorRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a handler for a transaction type.
    pub fn register<T: Transactor + 'static>(&mut self, tx_type: TransactionType, handler: T) {
        self.handlers.insert(tx_type, Box::new(handler));
    }

    /// Look up the handler for a transaction type.
    pub fn get(&self, tx_type: &TransactionType) -> Option<&dyn Transactor> {
        self.handlers.get(tx_type).map(|h| h.as_ref())
    }

    /// Check if a handler is registered for a transaction type.
    pub fn has(&self, tx_type: &TransactionType) -> bool {
        self.handlers.contains_key(tx_type)
    }
}

impl Default for TransactorRegistry {
    fn default() -> Self {
        Self::new()
    }
}
