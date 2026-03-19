use std::collections::{BTreeMap, HashMap};

use rxrpl_primitives::Hash256;
use serde_json::Value;

use crate::error::TxqError;
use crate::fee::FeeLevel;

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
    /// Maximum queue size.
    max_size: usize,
}

impl TxQueue {
    pub fn new(max_size: usize) -> Self {
        Self {
            by_fee: BTreeMap::new(),
            by_hash: HashMap::new(),
            by_account: HashMap::new(),
            max_size,
        }
    }

    /// Add a transaction to the queue.
    pub fn submit(&mut self, entry: QueueEntry) -> Result<(), TxqError> {
        if self.by_hash.contains_key(&entry.hash) {
            return Err(TxqError::Duplicate);
        }
        if self.by_hash.len() >= self.max_size {
            return Err(TxqError::QueueFull);
        }

        let hash = entry.hash;
        let fee_level = entry.fee_level;
        let account = entry.account.clone();

        self.by_fee.insert(std::cmp::Reverse(fee_level), hash);
        self.by_account.entry(account).or_default().push(hash);
        self.by_hash.insert(hash, entry);

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
        Some(entry)
    }

    /// Remove expired transactions.
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

        for hash in expired {
            self.remove(&hash);
        }
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
        q.submit(make_entry(0x01, 10, "alice")).unwrap();
        q.submit(make_entry(0x02, 20, "alice")).unwrap();
        q.submit(make_entry(0x03, 30, "bob")).unwrap();

        assert_eq!(q.account_txs("alice").len(), 2);
        assert_eq!(q.account_txs("bob").len(), 1);
        assert_eq!(q.account_txs("charlie").len(), 0);
    }
}
