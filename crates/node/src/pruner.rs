use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use rxrpl_ledger::Ledger;
use rxrpl_primitives::Hash256;
use rxrpl_rpc_server::PrunerState;
use rxrpl_shamap::NodeStore;

/// Maximum number of node hashes to delete in a single batch to avoid
/// blocking the storage backend for too long.
const DELETE_BATCH_SIZE: usize = 4096;

/// How often (in validated ledgers) automatic pruning is attempted.
const PRUNING_INTERVAL: u32 = 256;

/// Manages automatic deletion of old ledger history.
///
/// Tracks the earliest available ledger and the advisory deletion cursor.
/// After each validated ledger, checks whether the retention window has
/// been exceeded and, if so, prunes unreferenced SHAMap nodes.
pub struct LedgerPruner {
    /// Shared state accessible from both the close loop and RPC handlers.
    state: Arc<PrunerState>,
}

impl LedgerPruner {
    /// Create a new pruner with the given retention window.
    pub fn new(retention_window: u32, advisory_delete: bool) -> Self {
        Self {
            state: Arc::new(PrunerState::new(retention_window, advisory_delete)),
        }
    }

    /// Returns a reference to the shared pruner state for use by RPC handlers.
    pub fn shared_state(&self) -> Arc<PrunerState> {
        Arc::clone(&self.state)
    }

    /// Returns the earliest ledger sequence still available.
    pub fn earliest_seq(&self) -> u32 {
        self.state.earliest_seq.load(Ordering::Relaxed)
    }

    /// Returns the current advisory delete cursor.
    pub fn can_delete_seq(&self) -> u32 {
        self.state.can_delete_seq.load(Ordering::Relaxed)
    }

    /// Set the advisory delete cursor.
    pub fn set_can_delete(&self, seq: u32) {
        self.state.can_delete_seq.store(seq, Ordering::Relaxed);
    }

    /// Returns true if pruning is enabled (retention window > 0).
    pub fn is_enabled(&self) -> bool {
        self.state.retention_window > 0
    }

    /// Check whether pruning should run after the given validated sequence.
    pub fn should_prune(&self, validated_seq: u32) -> bool {
        if !self.is_enabled() {
            return false;
        }
        validated_seq % PRUNING_INTERVAL == 0
    }

    /// Prune old ledger nodes that are no longer within the retention window.
    ///
    /// `current_seq` is the latest validated ledger sequence.
    /// `old_ledgers` contains historical ledgers eligible for pruning
    /// (those older than the retention window).
    /// `retained_ledger` is the most recent ledger at the boundary of the
    /// retention window whose node hashes must be kept.
    ///
    /// Returns the number of nodes deleted.
    pub fn prune(
        &self,
        current_seq: u32,
        old_ledgers: &[Ledger],
        retained_ledger: Option<&Ledger>,
        store: &Arc<dyn NodeStore>,
    ) -> usize {
        if !self.is_enabled() || old_ledgers.is_empty() {
            return 0;
        }

        let cutoff_seq = current_seq.saturating_sub(self.state.retention_window);

        // Check advisory cursor
        let can_delete = self.state.can_delete_seq.load(Ordering::Relaxed);
        if self.state.advisory_delete && can_delete == 0 {
            return 0;
        }

        // Effective cutoff: min of retention-based cutoff and advisory cursor.
        let effective_cutoff = if self.state.advisory_delete && can_delete != u32::MAX {
            cutoff_seq.min(can_delete)
        } else {
            cutoff_seq
        };

        // Collect node hashes from the retained (live) ledger so we do not
        // accidentally delete them. State map nodes are content-addressed
        // and shared across ledgers.
        let mut live_hashes: HashSet<Hash256> = HashSet::new();
        if let Some(retained) = retained_ledger {
            for h in retained.state_map.collect_all_node_hashes() {
                live_hashes.insert(h);
            }
            for h in retained.tx_map.collect_all_node_hashes() {
                live_hashes.insert(h);
            }
        }

        // Collect node hashes from ledgers to be pruned
        let mut prunable_hashes: Vec<Hash256> = Vec::new();
        let mut new_earliest = self.state.earliest_seq.load(Ordering::Relaxed);

        for ledger in old_ledgers {
            let seq = ledger.header.sequence;
            if seq > effective_cutoff {
                continue;
            }

            for h in ledger.state_map.collect_all_node_hashes() {
                if !live_hashes.contains(&h) {
                    prunable_hashes.push(h);
                }
            }
            for h in ledger.tx_map.collect_all_node_hashes() {
                if !live_hashes.contains(&h) {
                    prunable_hashes.push(h);
                }
            }

            if seq >= new_earliest || new_earliest == 0 {
                new_earliest = seq + 1;
            }
        }

        // Deduplicate
        prunable_hashes.sort_unstable();
        prunable_hashes.dedup();

        // Remove any that are also live (belt-and-suspenders)
        prunable_hashes.retain(|h| !live_hashes.contains(h));

        let total = prunable_hashes.len();
        if total == 0 {
            return 0;
        }

        // Delete in batches
        for chunk in prunable_hashes.chunks(DELETE_BATCH_SIZE) {
            if let Err(e) = store.delete_batch(chunk) {
                tracing::warn!("pruner: failed to delete batch: {}", e);
                break;
            }
        }

        // Update earliest sequence
        if new_earliest > self.state.earliest_seq.load(Ordering::Relaxed) {
            self.state
                .earliest_seq
                .store(new_earliest, Ordering::Relaxed);
        }

        tracing::info!(
            "pruned {} nodes from ledgers up to #{}, earliest now #{}",
            total,
            effective_cutoff,
            new_earliest
        );

        metrics::counter!("pruner_nodes_deleted_total").increment(total as u64);
        metrics::gauge!("pruner_earliest_ledger").set(new_earliest as f64);

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruner_disabled_when_zero() {
        let pruner = LedgerPruner::new(0, false);
        assert!(!pruner.is_enabled());
        assert!(!pruner.should_prune(256));
    }

    #[test]
    fn pruner_enabled_with_window() {
        let pruner = LedgerPruner::new(2000, false);
        assert!(pruner.is_enabled());
        assert!(pruner.should_prune(256));
        assert!(!pruner.should_prune(100));
        assert!(pruner.should_prune(512));
    }

    #[test]
    fn advisory_delete_blocks_pruning() {
        let pruner = LedgerPruner::new(100, true);
        assert!(pruner.is_enabled());
        // Advisory mode starts with can_delete = 0 (never)
        assert_eq!(pruner.can_delete_seq(), 0);

        let store: Arc<dyn NodeStore> = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());
        let deleted = pruner.prune(500, &[], None, &store);
        assert_eq!(deleted, 0);

        // Advance advisory cursor
        pruner.set_can_delete(400);
        assert_eq!(pruner.can_delete_seq(), 400);
    }

    #[test]
    fn set_can_delete_always() {
        let pruner = LedgerPruner::new(100, true);
        pruner.set_can_delete(u32::MAX);
        assert_eq!(pruner.can_delete_seq(), u32::MAX);
    }

    #[test]
    fn earliest_seq_starts_at_zero() {
        let pruner = LedgerPruner::new(100, false);
        assert_eq!(pruner.earliest_seq(), 0);
    }

    #[test]
    fn prune_no_old_ledgers_returns_zero() {
        let pruner = LedgerPruner::new(100, false);
        let store: Arc<dyn NodeStore> = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());
        assert_eq!(pruner.prune(500, &[], None, &store), 0);
    }

    #[test]
    fn prune_with_empty_ledgers() {
        let pruner = LedgerPruner::new(10, false);
        let store: Arc<dyn NodeStore> = Arc::new(rxrpl_shamap::InMemoryNodeStore::new());

        // Create a minimal closed ledger with seq=1
        let mut ledger = Ledger::genesis();
        let _ = ledger.close(100, 0);

        // Current seq=20, retention=10, so cutoff=10. Ledger #1 is eligible.
        let deleted = pruner.prune(20, &[ledger], None, &store);
        // Genesis has no store-backed nodes, so nothing to delete from store
        assert_eq!(deleted, 0);
    }

    #[test]
    fn shared_state_syncs_with_pruner() {
        let pruner = LedgerPruner::new(500, true);
        let state = pruner.shared_state();

        // Initially advisory_delete = true, can_delete = 0
        assert_eq!(state.can_delete_seq.load(Ordering::Relaxed), 0);

        // Set via pruner
        pruner.set_can_delete(100);
        assert_eq!(state.can_delete_seq.load(Ordering::Relaxed), 100);

        // Set via shared state
        state.can_delete_seq.store(200, Ordering::Relaxed);
        assert_eq!(pruner.can_delete_seq(), 200);
    }

    #[test]
    fn prune_deletes_store_backed_nodes() {
        use rxrpl_shamap::{InMemoryNodeStore, SHAMap};

        let store: Arc<dyn NodeStore> = Arc::new(InMemoryNodeStore::new());
        let pruner = LedgerPruner::new(5, false);

        // Build a ledger with store-backed state and tx maps
        let mut state_map = SHAMap::account_state_with_store(Arc::clone(&store));
        state_map
            .insert(Hash256::new([0x01; 32]), vec![1, 2, 3])
            .unwrap();
        state_map
            .insert(Hash256::new([0x02; 32]), vec![4, 5, 6])
            .unwrap();
        // Flush to persist nodes in the store
        let _state_root = state_map.flush().unwrap();

        let mut tx_map = SHAMap::transaction_with_meta_and_store(Arc::clone(&store));
        tx_map
            .insert(Hash256::new([0xA1; 32]), vec![10, 20])
            .unwrap();
        let _tx_root = tx_map.flush().unwrap();

        // Collect hashes that we expect to find in the store
        let state_hashes = state_map.collect_all_node_hashes();
        let tx_hashes = tx_map.collect_all_node_hashes();
        assert!(!state_hashes.is_empty(), "state map should have nodes");
        assert!(!tx_hashes.is_empty(), "tx map should have nodes");

        // Verify nodes exist in store
        for h in &state_hashes {
            assert!(
                store.fetch(h).unwrap().is_some(),
                "state node {:?} should exist before pruning",
                h
            );
        }

        // Build a simple closed ledger referencing these maps
        let mut old_ledger = Ledger::genesis();
        old_ledger.state_map = state_map;
        old_ledger.tx_map = tx_map;
        let _ = old_ledger.close(100, 0);

        // Prune: current_seq=20, retention=5, cutoff=15. Ledger #1 is eligible.
        let deleted = pruner.prune(20, &[old_ledger], None, &store);
        assert!(deleted > 0, "should have deleted some nodes");

        // Verify that pruned state nodes are gone from the store
        for h in &state_hashes {
            assert!(
                store.fetch(h).unwrap().is_none(),
                "state node {:?} should be deleted after pruning",
                h
            );
        }

        // Verify earliest_seq was updated
        assert!(pruner.earliest_seq() > 0);
    }

    #[test]
    fn prune_preserves_retained_ledger_nodes() {
        use rxrpl_shamap::{InMemoryNodeStore, SHAMap};

        let store: Arc<dyn NodeStore> = Arc::new(InMemoryNodeStore::new());
        let pruner = LedgerPruner::new(5, false);

        // Shared data: both old and retained ledger reference the same key
        let shared_key = Hash256::new([0x55; 32]);
        let shared_data = vec![7, 8, 9];

        // Old ledger (seq=1)
        let mut old_state = SHAMap::account_state_with_store(Arc::clone(&store));
        old_state.insert(shared_key, shared_data.clone()).unwrap();
        old_state.insert(Hash256::new([0x11; 32]), vec![1]).unwrap();
        let _ = old_state.flush().unwrap();

        let mut old_ledger = Ledger::genesis();
        old_ledger.state_map = old_state;
        let _ = old_ledger.close(100, 0);

        // Retained ledger (seq=16) shares the same key
        let mut retained_state = SHAMap::account_state_with_store(Arc::clone(&store));
        retained_state
            .insert(shared_key, shared_data.clone())
            .unwrap();
        let _ = retained_state.flush().unwrap();

        let retained_hashes: HashSet<_> = retained_state
            .collect_all_node_hashes()
            .into_iter()
            .collect();

        let mut retained_ledger = Ledger::genesis();
        retained_ledger.state_map = retained_state;
        let _ = retained_ledger.close(200, 0);

        // Prune with retained ledger
        let _deleted = pruner.prune(20, &[old_ledger], Some(&retained_ledger), &store);

        // Nodes shared with the retained ledger must survive
        for h in &retained_hashes {
            assert!(
                store.fetch(h).unwrap().is_some(),
                "retained node {:?} should survive pruning",
                h
            );
        }
    }

    #[test]
    fn advisory_mode_respects_cursor() {
        use rxrpl_shamap::{InMemoryNodeStore, SHAMap};

        let store: Arc<dyn NodeStore> = Arc::new(InMemoryNodeStore::new());
        let pruner = LedgerPruner::new(5, true);

        // Build old ledger
        let mut state = SHAMap::account_state_with_store(Arc::clone(&store));
        state.insert(Hash256::new([0xBB; 32]), vec![1, 2]).unwrap();
        let _ = state.flush().unwrap();
        let hashes = state.collect_all_node_hashes();

        let mut old_ledger = Ledger::genesis();
        old_ledger.state_map = state;
        let _ = old_ledger.close(100, 0);

        // Advisory mode: can_delete = 0 (never) -- should not prune
        let deleted = pruner.prune(20, &[old_ledger.clone()], None, &store);
        assert_eq!(deleted, 0, "should not prune when advisory says never");

        // Now advance cursor to allow deletion
        pruner.set_can_delete(10);
        let deleted = pruner.prune(20, &[old_ledger], None, &store);
        assert!(deleted > 0, "should prune after advisory cursor advanced");

        // Verify nodes deleted
        for h in &hashes {
            assert!(store.fetch(h).unwrap().is_none());
        }
    }
}
