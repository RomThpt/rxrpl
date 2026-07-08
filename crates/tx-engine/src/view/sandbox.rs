use std::collections::HashMap;

use rxrpl_primitives::Hash256;

use crate::fees::FeeSettings;
use crate::view::apply_view::{ApplyCheckpoint, ApplyError, ApplyView};
use crate::view::read_view::ReadView;

/// Type-erased snapshot of a [`Sandbox`]'s mutation maps, carried inside an
/// [`ApplyCheckpoint`]. Cloned by `Sandbox::checkpoint` and restored by
/// `Sandbox::rollback`. Payment sandboxes hold only a few dozen SLEs, so the
/// clone is cheap.
struct SandboxSnapshot {
    inserts: HashMap<Hash256, Vec<u8>>,
    updates: HashMap<Hash256, Vec<u8>>,
    deletes: HashMap<Hash256, Vec<u8>>,
    originals: HashMap<Hash256, Vec<u8>>,
    destroyed_drops: u64,
}

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
    /// Whether the `SortedDirectories` amendment is active for this apply.
    sorted_directories: bool,
    /// Whether the `fixPreviousTxnID` amendment is active for this apply. Gates
    /// whether newly created directory-node pages are threaded.
    thread_directories: bool,
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
            sorted_directories: false,
            thread_directories: true,
        }
    }

    /// Record whether the `SortedDirectories` amendment is active. Set once per
    /// apply from the engine's `Rules`; inherited by child sandboxes.
    pub fn set_sorted_directories(&mut self, enabled: bool) {
        self.sorted_directories = enabled;
    }

    /// Record whether the `fixPreviousTxnID` amendment is active. Set once per
    /// apply from the engine's `Rules`; inherited by child sandboxes.
    pub fn set_thread_directories(&mut self, enabled: bool) {
        self.thread_directories = enabled;
    }

    /// Create a nested sandbox for speculative execution.
    pub fn child(&self) -> Sandbox<'_> {
        let mut c = Sandbox::new(self);
        c.sorted_directories = self.sorted_directories;
        c.thread_directories = self.thread_directories;
        c
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

    fn succ(&self, key: &Hash256) -> Option<Hash256> {
        // Walk the parent's successors, skipping entries deleted in this
        // sandbox, then fold in any sandbox-local insert that lands earlier.
        let mut candidate = self.parent.succ(key);
        while let Some(k) = candidate {
            if self.deletes.contains_key(&k) {
                candidate = self.parent.succ(&k);
            } else {
                break;
            }
        }
        for ik in self.inserts.keys() {
            if ik > key && candidate.is_none_or(|c| *ik < c) {
                candidate = Some(*ik);
            }
        }
        candidate
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

    fn close_time(&self) -> u32 {
        self.parent.close_time()
    }

    fn parent_hash(&self) -> rxrpl_primitives::Hash256 {
        self.parent.parent_hash()
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
    fn sorted_directories(&self) -> bool {
        self.sorted_directories
    }
    fn thread_directories(&self) -> bool {
        self.thread_directories
    }

    fn checkpoint(&self) -> ApplyCheckpoint {
        ApplyCheckpoint(Some(Box::new(SandboxSnapshot {
            inserts: self.inserts.clone(),
            updates: self.updates.clone(),
            deletes: self.deletes.clone(),
            originals: self.originals.clone(),
            destroyed_drops: self.destroyed_drops,
        })))
    }

    fn rollback(&mut self, cp: ApplyCheckpoint) {
        let Some(any) = cp.0 else { return };
        let Ok(snap) = any.downcast::<SandboxSnapshot>() else {
            return;
        };
        let snap = *snap;
        self.inserts = snap.inserts;
        self.updates = snap.updates;
        self.deletes = snap.deletes;
        self.originals = snap.originals;
        self.destroyed_drops = snap.destroyed_drops;
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
    /// Stamp `PreviousTxnID` / `PreviousTxnLgrSeq` on every created or modified
    /// entry that carries those fields.
    ///
    /// rippled records, on each touched SLE that has the `sfPreviousTxnID`
    /// field, the id of the transaction that last modified it and the ledger it
    /// was applied in. Handlers previously set this ad hoc (only Payment, and
    /// only on freshly created accounts — with a zero id), so every other
    /// modified entry diverged from rippled on these two fields. Doing it here,
    /// centrally, covers all transaction types. We only touch entries that
    /// already expose `PreviousTxnID` so field-less types (DirectoryNode,
    /// LedgerHashes, Amendments, ...) are left untouched.
    pub fn stamp_previous_txn(
        &mut self,
        tx_id_hex: &str,
        ledger_seq: u32,
    ) -> HashMap<Hash256, (serde_json::Value, serde_json::Value)> {
        // rippled records a modified node's node-level PreviousTxnID as the
        // value the SLE carried BEFORE this tx threaded it
        // (`ApplyStateTable::threadItem` -> `SLE::thread`). A directory emptied
        // then recreated in this tx carries only a fresh zero placeholder, so
        // its metadata must not surface the parent's PreviousTxnID. Capture each
        // updated node's pre-stamp value for the metadata builder to use.
        let mut pre_stamp = HashMap::new();
        for (key, data) in self.updates.iter_mut() {
            let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(data) else {
                continue;
            };
            let Some(obj) = v.as_object_mut() else {
                continue;
            };
            if !obj.contains_key("PreviousTxnID") {
                continue;
            }
            pre_stamp.insert(
                *key,
                (
                    obj.get("PreviousTxnID")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                    obj.get("PreviousTxnLgrSeq")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                ),
            );
            obj.insert(
                "PreviousTxnID".to_string(),
                serde_json::Value::String(tx_id_hex.to_string()),
            );
            obj.insert(
                "PreviousTxnLgrSeq".to_string(),
                serde_json::Value::Number(ledger_seq.into()),
            );
            if let Ok(bytes) = serde_json::to_vec(&v) {
                *data = bytes;
            }
        }
        for data in self.inserts.values_mut() {
            let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(data) else {
                continue;
            };
            let Some(obj) = v.as_object_mut() else {
                continue;
            };
            if !obj.contains_key("PreviousTxnID") {
                continue;
            }
            obj.insert(
                "PreviousTxnID".to_string(),
                serde_json::Value::String(tx_id_hex.to_string()),
            );
            obj.insert(
                "PreviousTxnLgrSeq".to_string(),
                serde_json::Value::Number(ledger_seq.into()),
            );
            if let Ok(bytes) = serde_json::to_vec(&v) {
                *data = bytes;
            }
        }
        pre_stamp
    }

    /// Thread `sfAccountTxnID` on the sender's account root to the id of the
    /// transaction being applied, mirroring rippled's base `Transactor::apply`:
    ///
    /// ```text
    /// if (sle->isFieldPresent(sfAccountTxnID))
    ///     sle->setFieldH256(sfAccountTxnID, ctx_.tx.getTransactionID());
    /// ```
    ///
    /// rippled updates the field ONLY when it is already present (the account
    /// opted in via `asfAccountTxnID`); accounts that never enabled it are left
    /// untouched. The sender root is always in the change set (its sequence was
    /// consumed), so we patch it in place here.
    pub fn thread_account_txn_id(&mut self, account_key: &Hash256, tx_id_hex: &str) {
        let Some(data) = self
            .updates
            .get_mut(account_key)
            .or_else(|| self.inserts.get_mut(account_key))
        else {
            return;
        };
        let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(data) else {
            return;
        };
        let Some(obj) = v.as_object_mut() else {
            return;
        };
        if !obj.contains_key("AccountTxnID") {
            return;
        }
        obj.insert(
            "AccountTxnID".to_string(),
            serde_json::Value::String(tx_id_hex.to_string()),
        );
        if let Ok(bytes) = serde_json::to_vec(&v) {
            *data = bytes;
        }
    }

    /// Apply these changes to a ledger.
    ///
    /// JSON data from handlers is encoded to XRPL binary before storage
    /// in the SHAMap, ensuring hash-compatible state with rippled.
    pub fn apply_to_ledger(
        self,
        ledger: &mut rxrpl_ledger::Ledger,
    ) -> Result<(), rxrpl_ledger::LedgerError> {
        for (key, data) in self.inserts {
            let binary = rxrpl_ledger::sle_codec::encode_sle(&data)
                .map_err(|e| rxrpl_ledger::LedgerError::Codec(e.to_string()))?;
            ledger.put_state(key, binary)?;
        }
        for (key, data) in self.updates {
            let binary = rxrpl_ledger::sle_codec::encode_sle(&data)
                .map_err(|e| rxrpl_ledger::LedgerError::Codec(e.to_string()))?;
            ledger.put_state(key, binary)?;
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

    #[test]
    fn checkpoint_rollback_restores_state() {
        let (mut ledger, _) = genesis_view();
        let existing = Hash256::new([0x10; 32]);
        ledger.put_state(existing, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        sandbox.update(existing, vec![2]).unwrap();

        // Checkpoint, then mutate further (insert + update + erase + destroy).
        let cp = sandbox.checkpoint();
        let inserted = Hash256::new([0x11; 32]);
        sandbox.insert(inserted, vec![9]).unwrap();
        sandbox.update(existing, vec![3]).unwrap();
        sandbox.erase(&existing).unwrap();
        sandbox.destroy_drops(123);
        assert!(!sandbox.exists(&existing));
        assert!(sandbox.exists(&inserted));
        assert_eq!(sandbox.destroyed_drops(), 123);

        // Rollback restores exactly the checkpointed state.
        sandbox.rollback(cp);
        assert_eq!(sandbox.read(&existing), Some(vec![2]));
        assert!(!sandbox.exists(&inserted));
        assert_eq!(sandbox.destroyed_drops(), 0);
    }

    #[test]
    fn reap_side_channel_survives_rollback() {
        // The reap pattern: a deletion threaded out as a side-channel is
        // re-applied AFTER rollback, so an unfunded-offer removal survives a
        // discarded speculative trial (matches rippled's ofrsToRm -> accumSandbox).
        let (mut ledger, _) = genesis_view();
        let offer = Hash256::new([0x20; 32]);
        ledger.put_state(offer, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        let cp = sandbox.checkpoint();
        // Speculative mutation that will be discarded.
        sandbox.update(offer, vec![7]).unwrap();
        // Side-channel reap recorded out of band.
        let reap = vec![offer];
        sandbox.rollback(cp);
        // The trial's mutation is gone...
        assert_eq!(sandbox.read(&offer), Some(vec![1]));
        // ...but the reap is re-applied permanently.
        for k in &reap {
            sandbox.erase(k).unwrap();
        }
        assert!(!sandbox.exists(&offer));
    }

    #[test]
    fn sandbox_succ_merges_parent_and_local() {
        // High-prefix keys sort after the genesis entries.
        let a = Hash256::new([0xF0; 32]);
        let b = Hash256::new([0xF2; 32]);
        let c = Hash256::new([0xF4; 32]);
        let (mut ledger, _) = genesis_view();
        ledger.put_state(a, vec![1]).unwrap();
        ledger.put_state(c, vec![1]).unwrap();

        let view = LedgerView::new(&ledger);
        let mut sandbox = Sandbox::new(&view);
        // Local insert b lands between parent's a and c.
        sandbox.insert(b, vec![1]).unwrap();

        let below = Hash256::new([0xEF; 32]);
        assert_eq!(sandbox.succ(&below), Some(a));
        assert_eq!(sandbox.succ(&a), Some(b), "local insert is found");
        assert_eq!(sandbox.succ(&b), Some(c));

        // Deleting c locally makes succ(b) skip past it.
        sandbox.erase(&c).unwrap();
        assert_eq!(sandbox.succ(&b), None);
    }
}
