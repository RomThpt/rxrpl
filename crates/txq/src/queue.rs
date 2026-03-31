use std::collections::{BTreeMap, HashMap};

use rxrpl_primitives::Hash256;
use serde_json::Value;

use crate::error::TxqError;
use crate::fee::{FeeLevel, MAX_ACCOUNT_QUEUE_DEPTH};

/// A queued transaction entry.
#[derive(Clone, Debug)]
pub struct QueueEntry {
    /// Transaction hash.
    pub hash: Hash256,
    /// Transaction JSON.
    pub tx: Value,
    /// Fee level for ordering.
    pub fee_level: FeeLevel,
    /// Account that submitted this transaction.
    pub account: String,
    /// Sequence number.
    pub sequence: u32,
    /// LastLedgerSequence (expiration).
    pub last_ledger_sequence: Option<u32>,
    /// Whether this transaction has already passed preflight checks.
    pub preflight_passed: bool,
}

/// Aggregate queue metrics for monitoring and reporting.
#[derive(Clone, Debug, Default)]
pub struct QueueMetrics {
    /// Total transactions ever queued.
    pub total_queued: u64,
    /// Total transactions successfully applied on retry.
    pub total_applied: u64,
    /// Total transactions expired (LastLedgerSequence exceeded).
    pub total_expired: u64,
    /// Total transactions dropped on retry failure.
    pub total_dropped: u64,
    /// Total transactions replaced via fee-bump.
    pub total_replaced: u64,
}

/// Transaction queue / mempool.
///
/// Orders transactions by fee level for processing.
/// Tracks per-account queues for sequence ordering.
pub struct TxQueue {
    /// All queued transactions, ordered by fee level (descending).
    by_fee: BTreeMap<std::cmp::Reverse<FeeLevel>, Hash256>,
    /// Transaction data by hash.
    by_hash: HashMap<Hash256, QueueEntry>,
    /// Per-account queues (account -> sequence-ordered hashes).
    by_account: HashMap<String, Vec<Hash256>>,
    /// Index from (account, sequence) to hash for fee replacement lookups.
    by_account_seq: HashMap<(String, u32), Hash256>,
    /// Maximum queue size.
    max_size: usize,
    /// Aggregate queue metrics.
    pub metrics: QueueMetrics,
}

impl TxQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            by_fee: BTreeMap::new(),
            by_hash: HashMap::new(),
            by_account: HashMap::new(),
            by_account_seq: HashMap::new(),
            max_size,
            metrics: QueueMetrics::default(),
        }
    }

    /// Maximum queue capacity.
    pub fn max_size(&self) -> usize {
        self.max_size
    }

    /// Add a transaction to the queue.
    ///
    /// Enforces the global queue size limit and a per-account depth limit
    /// of `MAX_ACCOUNT_QUEUE_DEPTH` (10, matching rippled).
    ///
    /// If a transaction with the same account+sequence already exists in the
    /// queue, the new transaction replaces it only if its fee level is strictly
    /// higher; otherwise `FeeTooLowForReplacement` is returned.
    pub fn submit(&mut self, entry: QueueEntry) -> Result<(), TxqError> {
        if self.by_hash.contains_key(&entry.hash) {
            return Err(TxqError::Duplicate);
        }

        // Fee replacement: same account + sequence already queued?
        let replacement_key = (entry.account.clone(), entry.sequence);
        if let Some(existing_hash) = self.by_account_seq.get(&replacement_key).copied() {
            if let Some(existing) = self.by_hash.get(&existing_hash) {
                if entry.fee_level <= existing.fee_level {
                    return Err(TxqError::FeeTooLowForReplacement);
                }
                // Remove the old entry to make room for the replacement.
                let old_hash = existing_hash;
                self.remove(&old_hash);
                self.metrics.total_replaced += 1;
                // Fall through to insert the new entry below.
            }
        }

        if self.by_hash.len() >= self.max_size {
            return Err(TxqError::QueueFull);
        }

        // Per-account depth limit
        let account_depth = self
            .by_account
            .get(&entry.account)
            .map(|v| v.len())
            .unwrap_or(0);
        if account_depth >= MAX_ACCOUNT_QUEUE_DEPTH {
            return Err(TxqError::AccountQueueFull);
        }

        let hash = entry.hash;
        let fee_level = entry.fee_level;
        let account = entry.account.clone();
        let sequence = entry.sequence;

        self.by_fee.insert(std::cmp::Reverse(fee_level), hash);
        self.by_account.entry(account.clone()).or_default().push(hash);
        self.by_account_seq.insert((account, sequence), hash);
        self.by_hash.insert(hash, entry);
        self.metrics.total_queued += 1;

        Ok(())
    }

    /// Remove a transaction by hash.
    pub fn remove(&mut self, hash: &Hash256) -> Option<QueueEntry> {
        let entry = self.by_hash.remove(hash)?;
        self.by_fee.retain(|_, h| h != hash);
        if let Some(acct_queue) = self.by_account.get_mut(&entry.account) {
            acct_queue.retain(|h| h != hash);
            if acct_queue.is_empty() {
                self.by_account.remove(&entry.account);
            }
        }
        self.by_account_seq.remove(&(entry.account.clone(), entry.sequence));
        Some(entry)
    }

    /// Remove expired transactions and update metrics.
    pub fn remove_expired(&mut self, current_ledger_seq: u32) {
        let expired: Vec<Hash256> = self
            .by_hash
            .iter()
            .filter(|(_, e)| {
                e.last_ledger_sequence
                    .is_some_and(|lls| lls < current_ledger_seq)
            })
            .map(|(h, _)| *h)
            .collect();

        let count = expired.len() as u64;
        for hash in expired {
            self.remove(&hash);
        }
        self.metrics.total_expired += count;
    }

    /// Get the highest-fee transaction.
    pub fn peek(&self) -> Option<&QueueEntry> {
        let (_, hash) = self.by_fee.iter().next()?;
        self.by_hash.get(hash)
    }

    /// Number of queued transactions.
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }

    /// Get all transaction hashes for an account.
    pub fn account_txs(&self, account: &str) -> &[Hash256] {
        self.by_account
            .get(account)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Look up a queue entry by hash.
    pub fn get(&self, hash: &Hash256) -> Option<&QueueEntry> {
        self.by_hash.get(hash)
    }

    /// Return all accounts that currently have queued transactions.
    pub fn accounts(&self) -> impl Iterator<Item = &String> {
        self.by_account.keys()
    }

    /// Drain all queued entries in fee-priority order for retry after ledger close.
    ///
    /// Returns a `Vec<QueueEntry>` sorted highest-fee-first so the caller can
    /// re-apply them against the new open ledger.
    pub fn drain_for_retry(&mut self) -> Vec<QueueEntry> {
        let mut entries: Vec<QueueEntry> = Vec::with_capacity(self.by_hash.len());
        // Iterate fee-descending (BTreeMap key is Reverse<FeeLevel>)
        for (_, hash) in self.by_fee.iter() {
            if let Some(entry) = self.by_hash.get(hash) {
                entries.push(entry.clone());
            }
        }
        // Clear all internal structures
        self.by_fee.clear();
        self.by_hash.clear();
        self.by_account.clear();
        self.by_account_seq.clear();
        entries
    }

    /// Drain all queued entries grouped by account, with each group sorted by
    /// sequence number ascending.
    ///
    /// Groups are returned in fee-descending order based on the highest-fee
    /// transaction in each group, ensuring high-value accounts are retried first.
    /// Within each account group, transactions are ordered by sequence so that
    /// they can be applied in the correct order against the new open ledger.
    pub fn drain_for_retry_ordered(&mut self) -> Vec<(String, Vec<QueueEntry>)> {
        // Collect all entries
        let all_entries: Vec<QueueEntry> = self.by_hash.values().cloned().collect();

        // Clear internal structures
        self.by_fee.clear();
        self.by_hash.clear();
        self.by_account.clear();
        self.by_account_seq.clear();

        // Group by account
        let mut by_account: HashMap<String, Vec<QueueEntry>> = HashMap::new();
        for entry in all_entries {
            by_account
                .entry(entry.account.clone())
                .or_default()
                .push(entry);
        }

        // Sort each group by sequence ascending
        for entries in by_account.values_mut() {
            entries.sort_by_key(|e| e.sequence);
        }

        // Sort groups by max fee level descending
        let mut groups: Vec<(String, Vec<QueueEntry>)> = by_account.into_iter().collect();
        groups.sort_by(|a, b| {
            let max_a = a.1.iter().map(|e| e.fee_level).max().unwrap_or(FeeLevel::new(0, 1));
            let max_b = b.1.iter().map(|e| e.fee_level).max().unwrap_or(FeeLevel::new(0, 1));
            max_b.cmp(&max_a)
        });

        groups
    }

    /// Record a dropped transaction in metrics.
    pub fn record_drop(&mut self) {
        self.metrics.total_dropped += 1;
    }

    /// Record a successfully applied transaction in metrics.
    pub fn record_applied(&mut self) {
        self.metrics.total_applied += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(hash_byte: u8, fee: u64, account: &str) -> QueueEntry {
        QueueEntry {
            hash: Hash256::new([hash_byte; 32]),
            tx: serde_json::json!({}),
            fee_level: FeeLevel::new(fee, 10),
            account: account.to_string(),
            sequence: 1,
            last_ledger_sequence: None,
            preflight_passed: false,
        }
    }

    fn make_entry_with_seq(hash_byte: u8, fee: u64, account: &str, seq: u32) -> QueueEntry {
        QueueEntry {
            hash: Hash256::new([hash_byte; 32]),
            tx: serde_json::json!({}),
            fee_level: FeeLevel::new(fee, 10),
            account: account.to_string(),
            sequence: seq,
            last_ledger_sequence: None,
            preflight_passed: false,
        }
    }

    #[test]
    fn submit_and_peek() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        q.submit(make_entry(0x02, 20, "bob")).unwrap();

        assert_eq!(q.len(), 2);
        // Highest fee first
        assert_eq!(q.peek().unwrap().hash, Hash256::new([0x02; 32]));
    }

    #[test]
    fn duplicate_rejected() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        assert!(matches!(
            q.submit(make_entry(0x01, 10, "alice")),
            Err(TxqError::Duplicate)
        ));
    }

    #[test]
    fn queue_full() {
        let mut q = TxQueue::new(1);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        assert!(matches!(
            q.submit(make_entry(0x02, 10, "bob")),
            Err(TxqError::QueueFull)
        ));
    }

    #[test]
    fn remove_transaction() {
        let mut q = TxQueue::new(10);
        let hash = Hash256::new([0x01; 32]);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        assert!(q.remove(&hash).is_some());
        assert!(q.is_empty());
    }

    #[test]
    fn remove_expired() {
        let mut q = TxQueue::new(10);
        let mut entry = make_entry(0x01, 10, "alice");
        entry.last_ledger_sequence = Some(100);
        q.submit(entry).unwrap();

        q.remove_expired(101);
        assert!(q.is_empty());
    }

    #[test]
    fn account_txs() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();
        q.submit(make_entry_with_seq(0x02, 20, "alice", 2)).unwrap();
        q.submit(make_entry_with_seq(0x03, 30, "bob", 1)).unwrap();

        assert_eq!(q.account_txs("alice").len(), 2);
        assert_eq!(q.account_txs("bob").len(), 1);
        assert_eq!(q.account_txs("charlie").len(), 0);
    }

    #[test]
    fn account_queue_limit() {
        use crate::fee::MAX_ACCOUNT_QUEUE_DEPTH;
        let mut q = TxQueue::new(100);
        for i in 0..MAX_ACCOUNT_QUEUE_DEPTH {
            q.submit(make_entry_with_seq(i as u8, 10 + i as u64, "alice", i as u32))
                .unwrap();
        }
        // The next one from the same account must fail.
        let result = q.submit(make_entry_with_seq(0xFF, 10, "alice", 99));
        assert!(matches!(result, Err(TxqError::AccountQueueFull)));

        // A different account should still work.
        q.submit(make_entry(0xFE, 10, "bob")).unwrap();
    }

    #[test]
    fn max_size_getter() {
        let q = TxQueue::new(42);
        assert_eq!(q.max_size(), 42);
    }

    #[test]
    fn drain_for_retry_returns_fee_ordered() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        q.submit(make_entry(0x02, 30, "bob")).unwrap();
        q.submit(make_entry(0x03, 20, "charlie")).unwrap();

        let entries = q.drain_for_retry();
        assert_eq!(entries.len(), 3);
        // Highest fee first
        assert_eq!(entries[0].hash, Hash256::new([0x02; 32]));
        assert_eq!(entries[1].hash, Hash256::new([0x03; 32]));
        assert_eq!(entries[2].hash, Hash256::new([0x01; 32]));

        // Queue must be empty afterwards
        assert!(q.is_empty());
    }

    // --- Fee replacement tests ---

    #[test]
    fn fee_replacement_same_account_sequence() {
        let mut q = TxQueue::new(10);
        // Submit initial tx with seq 1
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();
        assert_eq!(q.len(), 1);

        // Replace with higher fee
        q.submit(make_entry_with_seq(0x02, 20, "alice", 1)).unwrap();
        assert_eq!(q.len(), 1);
        // The old hash should be gone
        assert!(q.get(&Hash256::new([0x01; 32])).is_none());
        // The new hash should be present
        assert!(q.get(&Hash256::new([0x02; 32])).is_some());
        assert_eq!(q.metrics.total_replaced, 1);
    }

    #[test]
    fn fee_replacement_rejected_when_lower() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry_with_seq(0x01, 20, "alice", 1)).unwrap();

        // Try to replace with lower fee
        let result = q.submit(make_entry_with_seq(0x02, 10, "alice", 1));
        assert!(matches!(result, Err(TxqError::FeeTooLowForReplacement)));
        assert_eq!(q.len(), 1);
        // Original should still be present
        assert!(q.get(&Hash256::new([0x01; 32])).is_some());
    }

    #[test]
    fn fee_replacement_rejected_when_equal() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();

        let result = q.submit(make_entry_with_seq(0x02, 10, "alice", 1));
        assert!(matches!(result, Err(TxqError::FeeTooLowForReplacement)));
    }

    // --- Sequence-ordered retry tests ---

    #[test]
    fn drain_for_retry_ordered_groups_by_account() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();
        q.submit(make_entry_with_seq(0x02, 20, "alice", 2)).unwrap();
        q.submit(make_entry_with_seq(0x03, 30, "bob", 1)).unwrap();

        let groups = q.drain_for_retry_ordered();
        assert_eq!(groups.len(), 2);
        assert!(q.is_empty());

        // Bob has higher fee, should come first
        assert_eq!(groups[0].0, "bob");
        assert_eq!(groups[0].1.len(), 1);

        // Alice's txs should be sequence-ordered
        assert_eq!(groups[1].0, "alice");
        assert_eq!(groups[1].1.len(), 2);
        assert_eq!(groups[1].1[0].sequence, 1);
        assert_eq!(groups[1].1[1].sequence, 2);
    }

    #[test]
    fn drain_for_retry_ordered_sequences_within_account() {
        let mut q = TxQueue::new(10);
        // Insert out of sequence order
        q.submit(make_entry_with_seq(0x03, 10, "alice", 3)).unwrap();
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();
        q.submit(make_entry_with_seq(0x02, 10, "alice", 2)).unwrap();

        let groups = q.drain_for_retry_ordered();
        assert_eq!(groups.len(), 1);
        let (account, entries) = &groups[0];
        assert_eq!(account, "alice");
        assert_eq!(entries[0].sequence, 1);
        assert_eq!(entries[1].sequence, 2);
        assert_eq!(entries[2].sequence, 3);
    }

    // --- Metrics tests ---

    #[test]
    fn metrics_tracking_queued() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        q.submit(make_entry(0x02, 20, "bob")).unwrap();
        assert_eq!(q.metrics.total_queued, 2);
    }

    #[test]
    fn metrics_tracking_expired() {
        let mut q = TxQueue::new(10);
        let mut entry = make_entry(0x01, 10, "alice");
        entry.last_ledger_sequence = Some(100);
        q.submit(entry).unwrap();

        q.remove_expired(101);
        assert_eq!(q.metrics.total_expired, 1);
    }

    #[test]
    fn metrics_tracking_replaced() {
        let mut q = TxQueue::new(10);
        q.submit(make_entry_with_seq(0x01, 10, "alice", 1)).unwrap();
        q.submit(make_entry_with_seq(0x02, 20, "alice", 1)).unwrap();
        assert_eq!(q.metrics.total_replaced, 1);
    }

    #[test]
    fn metrics_tracking_dropped_and_applied() {
        let mut q = TxQueue::new(10);
        q.record_drop();
        q.record_drop();
        q.record_applied();
        assert_eq!(q.metrics.total_dropped, 2);
        assert_eq!(q.metrics.total_applied, 1);
    }

    // --- Preflight cache tests ---

    #[test]
    fn preflight_passed_defaults_false() {
        let entry = make_entry(0x01, 10, "alice");
        assert!(!entry.preflight_passed);
    }

    #[test]
    fn preflight_passed_persists_in_queue() {
        let mut q = TxQueue::new(10);
        let mut entry = make_entry(0x01, 10, "alice");
        entry.preflight_passed = true;
        q.submit(entry).unwrap();

        let hash = Hash256::new([0x01; 32]);
        assert!(q.get(&hash).unwrap().preflight_passed);
    }

    // --- Error variant tests ---

    #[test]
    fn error_display_fee_too_low_for_replacement() {
        let err = TxqError::FeeTooLowForReplacement;
        assert!(err.to_string().contains("fee too low for replacement"));
    }

    #[test]
    fn error_display_sequence_gap() {
        let err = TxqError::SequenceGap;
        assert!(err.to_string().contains("sequence gap"));
    }
}
