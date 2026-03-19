use rxrpl_primitives::Hash256;
use rxrpl_shamap::SHAMap;

use crate::error::LedgerError;
use crate::header::{INITIAL_XRP_DROPS, LedgerHeader};

/// The lifecycle state of a ledger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerState {
    /// Open for modifications (transactions can be applied).
    Open,
    /// Closed (hashes computed, no more modifications).
    Closed,
    /// Validated by consensus (fully immutable).
    Validated,
}

/// An XRPL ledger containing a header and two SHAMaps.
///
/// - `state_map`: Account state entries (keyed by ledger index / keylet)
/// - `tx_map`: Transactions and metadata
///
/// A ledger progresses through states: Open -> Closed -> Validated.
#[derive(Clone, Debug)]
pub struct Ledger {
    pub header: LedgerHeader,
    pub state_map: SHAMap,
    pub tx_map: SHAMap,
    state: LedgerState,
    /// Total drops destroyed (fees) since this ledger was opened.
    destroyed_drops: u64,
}

impl Ledger {
    /// Create a new open ledger derived from a parent.
    pub fn new_open(parent: &Ledger) -> Ledger {
        assert_eq!(parent.state, LedgerState::Closed, "parent must be closed");

        let mut header = LedgerHeader::new();
        header.sequence = parent.header.sequence + 1;
        header.parent_hash = parent.header.hash;
        header.parent_close_time = parent.header.close_time;
        header.drops = parent.header.drops;
        header.close_time_resolution = parent.header.close_time_resolution;

        // Mutable copy of parent state, fresh tx map
        let state_map = parent.state_map.mutable_copy();
        let tx_map = SHAMap::transaction_with_meta();

        Ledger {
            header,
            state_map,
            tx_map,
            state: LedgerState::Open,
            destroyed_drops: 0,
        }
    }

    /// Create the genesis ledger.
    pub fn genesis() -> Ledger {
        let mut header = LedgerHeader::new();
        header.sequence = 1;
        header.drops = INITIAL_XRP_DROPS;
        header.parent_hash = Hash256::ZERO;
        header.parent_close_time = 0;
        header.close_time = 0;
        header.close_time_resolution = 30;
        header.close_flags = 0;

        let state_map = SHAMap::account_state();
        let tx_map = SHAMap::transaction_with_meta();

        Ledger {
            header,
            state_map,
            tx_map,
            state: LedgerState::Open,
            destroyed_drops: 0,
        }
    }

    /// Return the current state of this ledger.
    pub fn state(&self) -> LedgerState {
        self.state
    }

    /// Return true if this ledger is open for modifications.
    pub fn is_open(&self) -> bool {
        self.state == LedgerState::Open
    }

    /// Return true if this ledger is closed.
    pub fn is_closed(&self) -> bool {
        self.state == LedgerState::Closed
    }

    /// Return true if this ledger has been validated.
    pub fn is_validated(&self) -> bool {
        self.state == LedgerState::Validated
    }

    /// Insert or update a state entry.
    pub fn put_state(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), LedgerError> {
        if !self.is_open() {
            return Err(LedgerError::Immutable);
        }
        self.state_map.put(key, data)?;
        Ok(())
    }

    /// Get a state entry by key.
    pub fn get_state(&self, key: &Hash256) -> Option<&[u8]> {
        self.state_map.get(key)
    }

    /// Check if a state entry exists.
    pub fn has_state(&self, key: &Hash256) -> bool {
        self.state_map.has(key)
    }

    /// Delete a state entry.
    pub fn delete_state(&mut self, key: &Hash256) -> Result<Vec<u8>, LedgerError> {
        if !self.is_open() {
            return Err(LedgerError::Immutable);
        }
        Ok(self.state_map.delete(key)?)
    }

    /// Add a transaction to the tx map.
    pub fn add_transaction(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), LedgerError> {
        if !self.is_open() {
            return Err(LedgerError::Immutable);
        }
        self.tx_map.put(key, data)?;
        Ok(())
    }

    /// Record destroyed XRP (transaction fees).
    pub fn destroy_drops(&mut self, drops: u64) -> Result<(), LedgerError> {
        if !self.is_open() {
            return Err(LedgerError::Immutable);
        }
        self.destroyed_drops += drops;
        Ok(())
    }

    /// Close this ledger, computing final hashes.
    pub fn close(&mut self, close_time: u32, close_flags: u8) -> Result<(), LedgerError> {
        if !self.is_open() {
            return Err(LedgerError::AlreadyClosed);
        }

        // Compute tree hashes
        self.header.account_hash = self.state_map.root_hash();
        self.header.tx_hash = self.tx_map.root_hash();

        // Apply destroyed drops
        self.header.drops = self.header.drops.saturating_sub(self.destroyed_drops);

        // Set close time
        self.header.close_time = close_time;
        self.header.close_flags = close_flags;

        // Compute ledger hash
        self.header.hash = self.header.compute_hash();

        // Make maps immutable
        self.state_map.set_immutable();
        self.tx_map.set_immutable();

        self.state = LedgerState::Closed;
        Ok(())
    }

    /// Mark this ledger as validated by consensus.
    pub fn set_validated(&mut self) -> Result<(), LedgerError> {
        if !self.is_closed() {
            return Err(LedgerError::NotClosed);
        }
        self.state = LedgerState::Validated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_ledger() {
        let genesis = Ledger::genesis();
        assert!(genesis.is_open());
        assert_eq!(genesis.header.sequence, 1);
        assert_eq!(genesis.header.drops, INITIAL_XRP_DROPS);
        assert_eq!(genesis.header.parent_hash, Hash256::ZERO);
    }

    #[test]
    fn close_genesis() {
        let mut genesis = Ledger::genesis();
        genesis.close(0, 0).unwrap();
        assert!(genesis.is_closed());
        assert!(!genesis.header.hash.is_zero());
    }

    #[test]
    fn close_twice_fails() {
        let mut genesis = Ledger::genesis();
        genesis.close(0, 0).unwrap();
        assert_eq!(genesis.close(0, 0), Err(LedgerError::AlreadyClosed));
    }

    #[test]
    fn validate() {
        let mut genesis = Ledger::genesis();
        genesis.close(0, 0).unwrap();
        genesis.set_validated().unwrap();
        assert!(genesis.is_validated());
    }

    #[test]
    fn validate_open_fails() {
        let mut genesis = Ledger::genesis();
        assert_eq!(genesis.set_validated(), Err(LedgerError::NotClosed));
    }

    #[test]
    fn new_open_from_closed() {
        let mut genesis = Ledger::genesis();
        let key = Hash256::new([0xAA; 32]);
        genesis.put_state(key, vec![1, 2, 3]).unwrap();
        genesis.close(0, 0).unwrap();

        let child = Ledger::new_open(&genesis);
        assert!(child.is_open());
        assert_eq!(child.header.sequence, 2);
        assert_eq!(child.header.parent_hash, genesis.header.hash);
        // State from parent is inherited
        assert_eq!(child.get_state(&key), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn state_operations() {
        let mut ledger = Ledger::genesis();
        let key = Hash256::new([0xBB; 32]);

        // Put
        ledger.put_state(key, vec![10]).unwrap();
        assert_eq!(ledger.get_state(&key), Some(&[10][..]));
        assert!(ledger.has_state(&key));

        // Update
        ledger.put_state(key, vec![20]).unwrap();
        assert_eq!(ledger.get_state(&key), Some(&[20][..]));

        // Delete
        let old = ledger.delete_state(&key).unwrap();
        assert_eq!(old, vec![20]);
        assert!(!ledger.has_state(&key));
    }

    #[test]
    fn closed_ledger_rejects_writes() {
        let mut ledger = Ledger::genesis();
        ledger.close(0, 0).unwrap();

        let key = Hash256::new([0xCC; 32]);
        assert_eq!(ledger.put_state(key, vec![1]), Err(LedgerError::Immutable));
        assert_eq!(
            ledger.add_transaction(key, vec![1]),
            Err(LedgerError::Immutable)
        );
    }

    #[test]
    fn destroy_drops() {
        let mut ledger = Ledger::genesis();
        ledger.destroy_drops(1000).unwrap();
        ledger.close(0, 0).unwrap();
        assert_eq!(ledger.header.drops, INITIAL_XRP_DROPS - 1000);
    }

    #[test]
    fn ledger_hash_deterministic() {
        let mut l1 = Ledger::genesis();
        l1.put_state(Hash256::new([0x01; 32]), vec![1]).unwrap();
        l1.close(100, 0).unwrap();

        let mut l2 = Ledger::genesis();
        l2.put_state(Hash256::new([0x01; 32]), vec![1]).unwrap();
        l2.close(100, 0).unwrap();

        assert_eq!(l1.header.hash, l2.header.hash);
    }

    #[test]
    fn child_modifications_independent() {
        let mut genesis = Ledger::genesis();
        let key = Hash256::new([0xDD; 32]);
        genesis.put_state(key, vec![1]).unwrap();
        genesis.close(0, 0).unwrap();

        let mut child = Ledger::new_open(&genesis);
        child.put_state(key, vec![2]).unwrap();

        // Parent data is unchanged
        assert_eq!(genesis.get_state(&key), Some(&[1][..]));
        assert_eq!(child.get_state(&key), Some(&[2][..]));
    }
}
