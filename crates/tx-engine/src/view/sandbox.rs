use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::fees::FeeSettings;
use crate::view::apply_view::{ApplyError, ApplyView};
use crate::view::read_view::ReadView;

/// A copy-on-write mutation layer over a `ReadView`.
///
/// Tracks inserts, updates, and deletes. Can be committed to the
/// underlying ledger or discarded. Supports nesting for speculative
/// execution (e.g., offer crossing).
pub struct Sandbox<'a> {
    parent: &'a dyn ReadView,
    inserts: HashMap<Hash256, Vec<u8>>,
    updates: HashMap<Hash256, Vec<u8>>,
    deletes: HashMap<Hash256, Vec<u8>>,
    /// Pre-mutation values for entries that existed before this sandbox.
    /// Captured on first update/erase so TxMeta can show previous fields.
    originals: HashMap<Hash256, Vec<u8>>,
    destroyed_drops: u64,
}

impl<'a> Sandbox<'a> {
    /// Create a sandbox over a read view.
    pub fn new(parent: &'a dyn ReadView) -> Self {
        Self {
            parent,
            inserts: HashMap::new(),
            updates: HashMap::new(),
            deletes: HashMap::new(),
            originals: HashMap::new(),
            destroyed_drops: 0,
        }
    }

    /// Create a nested sandbox for speculative execution.
    pub fn child(&self) -> Sandbox<'_> {
        Sandbox::new(self)
    }

    /// Get the total drops destroyed in this sandbox.
    pub fn destroyed_drops(&self) -> u64 {
        self.destroyed_drops
    }

    /// Merge changes from a child sandbox into this sandbox.
    ///
    /// Used after a child sandbox succeeds (tes) to fold its mutations
    /// back into the parent before committing.
    pub fn merge_child_changes(&mut self, changes: SandboxChanges) {
        for (key, data) in changes.inserts {
            if self.deletes.remove(&key).is_some() {
                // Re-inserting something deleted in this sandbox -> update
                self.updates.insert(key, data);
            } else {
                self.inserts.insert(key, data);
            }
        }
        for (key, data) in changes.updates {
            if let Some(entry) = self.inserts.get_mut(&key) {
                // Updating something inserted in this sandbox
                *entry = data;
            } else {
                self.updates.insert(key, data);
            }
        }
        for (key, data) in changes.deletes {
            if self.inserts.remove(&key).is_some() {
                // Deleting something inserted in this sandbox -> no-op
            } else {
                self.updates.remove(&key);
                self.deletes.insert(key, data);
            }
        }
        self.destroyed_drops += changes.destroyed_drops;
        // Merge originals -- only keep the earliest original per key
        for (key, data) in changes.originals {
            self.originals.entry(key).or_insert(data);
        }
    }

    /// Consume this sandbox and return its changes.
    pub fn into_changes(self) -> SandboxChanges {
        SandboxChanges {
            inserts: self.inserts,
            updates: self.updates,
            deletes: self.deletes,
            originals: self.originals,
            destroyed_drops: self.destroyed_drops,
        }
    }
}

impl ReadView for Sandbox<'_> {
    fn read(&self, key: &Hash256) -> Option<Vec<u8>> {
        // Check deletes first
        if self.deletes.contains_key(key) {
            return None;
        }
        // Check updates
        if let Some(data) = self.updates.get(key) {
            return Some(data.clone());
        }
        // Check inserts
        if let Some(data) = self.inserts.get(key) {
            return Some(data.clone());
        }
        // Fall through to parent
        self.parent.read(key)
    }

    fn exists(&self, key: &Hash256) -> bool {
        if self.deletes.contains_key(key) {
            return false;
        }
        if self.updates.contains_key(key) || self.inserts.contains_key(key) {
            return true;
        }
        self.parent.exists(key)
    }

    fn seq(&self) -> u32 {
        self.parent.seq()
    }

    fn fees(&self) -> &FeeSettings {
        self.parent.fees()
    }

    fn drops(&self) -> u64 {
        self.parent.drops().saturating_sub(self.destroyed_drops)
    }

    fn parent_close_time(&self) -> u32 {
        self.parent.parent_close_time()
    }
}

impl ApplyView for Sandbox<'_> {
    fn insert(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), ApplyError> {
        if self.exists(&key) {
            return Err(ApplyError::AlreadyExists);
        }
        // If it was previously deleted, treat as update to restore
        if self.deletes.remove(&key).is_some() {
            self.updates.insert(key, data);
        } else {
            self.inserts.insert(key, data);
        }
        Ok(())
    }

    fn update(&mut self, key: Hash256, data: Vec<u8>) -> Result<(), ApplyError> {
        if !self.exists(&key) {
            return Err(ApplyError::NotFound);
        }
        if let Some(entry) = self.inserts.get_mut(&key) {
            // Updating something we inserted in this sandbox
            *entry = data;
        } else {
            // Capture original value on first modification
            if !self.originals.contains_key(&key) {
                if let Some(original) = self.parent.read(&key) {
                    self.originals.insert(key, original);
                }
            }
            self.updates.insert(key, data);
        }
        Ok(())
    }

    fn erase(&mut self, key: &Hash256) -> Result<(), ApplyError> {
        if !self.exists(key) {
            return Err(ApplyError::NotFound);
        }
        // If it was inserted in this sandbox, just remove the insert
        if self.inserts.remove(key).is_some() {
            return Ok(());
        }
        // Capture original value before deletion
        if !self.originals.contains_key(key) {
            if let Some(original) = self.parent.read(key) {
                self.originals.insert(*key, original);
            }
        }
        // Get the current data for the delete record
        let data = self.read(key).unwrap_or_default();
        self.updates.remove(key);
        self.deletes.insert(*key, data);
        Ok(())
    }

    fn destroy_drops(&mut self, drops: u64) {
        self.destroyed_drops += drops;
    }
}

/// The accumulated changes from a sandbox.
#[derive(Debug)]
pub struct SandboxChanges {
    pub inserts: HashMap<Hash256, Vec<u8>>,
    pub updates: HashMap<Hash256, Vec<u8>>,
    pub deletes: HashMap<Hash256, Vec<u8>>,
    /// Pre-mutation values for modified/deleted entries.
    pub originals: HashMap<Hash256, Vec<u8>>,
    pub destroyed_drops: u64,
}

impl SandboxChanges {
    /// Apply these changes to a ledger.
    pub fn apply_to_ledger(
        self,
        ledger: &mut rxrpl_ledger::Ledger,
    ) -> Result<(), rxrpl_ledger::LedgerError> {
        for (key, data) in self.inserts {
            ledger.put_state(key, data)?;
        }
        for (key, data) in self.updates {
            ledger.put_state(key, data)?;
        }
        for (key, _) in self.deletes {
            ledger.delete_state(&key)?;
        }
        if self.destroyed_drops > 0 {
            ledger.destroy_drops(self.destroyed_drops)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::ledger_view::LedgerView;
    use rxrpl_ledger::Ledger;

    fn genesis_view() -> (Ledger, FeeSettings) {
        (Ledger::genesis(), FeeSettings::default())
    }

    #[test]
    fn sandbox_read_through() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xAA; 32]);
        ledger.put_state(key, vec![1, 2, 3]).unwrap();

        let view = LedgerView::new(&ledger);
        let sandbox = Sandbox::new(&view);
        assert_eq!(sandbox.read(&key), Some(vec![1, 2, 3]));
    }

    #[test]
    fn sandbox_insert() {
        let (ledger, _) = genesis_view();
        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);

        let key = Hash256::new([0xBB; 32]);
        sandbox.insert(key, vec![10]).unwrap();
        assert_eq!(sandbox.read(&key), Some(vec![10]));
        assert!(sandbox.exists(&key));
    }

    #[test]
    fn sandbox_insert_duplicate_fails() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xCC; 32]);
        ledger.put_state(key, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        assert_eq!(sandbox.insert(key, vec![2]), Err(ApplyError::AlreadyExists));
    }

    #[test]
    fn sandbox_update() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xDD; 32]);
        ledger.put_state(key, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.update(key, vec![2]).unwrap();
        assert_eq!(sandbox.read(&key), Some(vec![2]));
    }

    #[test]
    fn sandbox_erase() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xEE; 32]);
        ledger.put_state(key, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.erase(&key).unwrap();
        assert!(!sandbox.exists(&key));
    }

    #[test]
    fn sandbox_insert_then_erase() {
        let (ledger, _) = genesis_view();
        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);

        let key = Hash256::new([0xFF; 32]);
        sandbox.insert(key, vec![1]).unwrap();
        sandbox.erase(&key).unwrap();
        assert!(!sandbox.exists(&key));
    }

    #[test]
    fn sandbox_erase_then_insert() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xAB; 32]);
        ledger.put_state(key, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.erase(&key).unwrap();
        sandbox.insert(key, vec![2]).unwrap();
        assert_eq!(sandbox.read(&key), Some(vec![2]));
    }

    #[test]
    fn sandbox_destroy_drops() {
        let (ledger, _) = genesis_view();
        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.destroy_drops(1000);
        assert_eq!(sandbox.destroyed_drops(), 1000);
    }

    #[test]
    fn nested_sandbox() {
        let (mut ledger, _) = genesis_view();
        let key = Hash256::new([0xAA; 32]);
        ledger.put_state(key, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.update(key, vec![2]).unwrap();

        let child = sandbox.child();
        assert_eq!(child.read(&key), Some(vec![2]));
    }

    #[test]
    fn apply_changes_to_ledger() {
        let (mut ledger, _) = genesis_view();
        let k1 = Hash256::new([0x01; 32]);
        let k2 = Hash256::new([0x02; 32]);
        ledger.put_state(k2, vec![20]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.insert(k1, vec![10]).unwrap();
        sandbox.erase(&k2).unwrap();
        sandbox.destroy_drops(500);

        let changes = sandbox.into_changes();
        changes.apply_to_ledger(&mut ledger).unwrap();

        assert_eq!(ledger.get_state(&k1), Some(&[10][..]));
        assert!(!ledger.has_state(&k2));
    }
}
