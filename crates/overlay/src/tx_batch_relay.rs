//! Batch transaction relay protocol (HaveTransactions / Transactions).
//!
//! rippled uses message types 63 (TMHaveTransactions) and 64 (TMTransactions)
//! to efficiently propagate transactions in batches rather than relaying each
//! transaction individually.
//!
//! Protocol flow:
//! 1. A node receives new transactions (via submit or from peers).
//! 2. It accumulates tx hashes and periodically broadcasts TMHaveTransactions
//!    containing the hashes of transactions it has.
//! 3. Peers receiving TMHaveTransactions check which hashes they lack, then
//!    respond with TMHaveTransactions containing only the missing hashes
//!    (acting as a request).
//! 4. The original node receives those "request" hashes and sends back
//!    TMTransactions with the full transaction data.

use std::num::NonZeroUsize;
use std::sync::Mutex;

use lru::LruCache;
use prost::Message;
use rxrpl_p2p_proto::proto::{TmHaveTransactions, TmTransaction, TmTransactions};
use rxrpl_primitives::Hash256;

/// Maximum number of transaction hashes in a single HaveTransactions message.
pub const MAX_BATCH_SIZE: usize = 256;

/// Tracks pending transaction data for batch relay.
///
/// When we receive or submit transactions, we store them here keyed by hash.
/// When a peer requests transactions (via TMHaveTransactions containing hashes
/// it wants), we look them up and send the data via TMTransactions.
pub struct TxBatchRelay {
    /// Known transaction data by hash, used to serve requests from peers.
    known_txs: Mutex<LruCache<Hash256, Vec<u8>>>,
    /// Hashes we have already requested from peers, to avoid duplicate requests.
    pending_requests: Mutex<LruCache<Hash256, ()>>,
    /// Accumulated hashes for the next outbound HaveTransactions broadcast.
    outbound_queue: Mutex<Vec<Hash256>>,
}

impl TxBatchRelay {
    pub fn new() -> Self {
        let known_cap = NonZeroUsize::new(8192).unwrap();
        let pending_cap = NonZeroUsize::new(4096).unwrap();
        Self {
            known_txs: Mutex::new(LruCache::new(known_cap)),
            pending_requests: Mutex::new(LruCache::new(pending_cap)),
            outbound_queue: Mutex::new(Vec::new()),
        }
    }

    /// Record a transaction we know about (received from peer or submitted locally).
    /// Returns true if this is a new transaction we haven't seen before.
    pub fn add_known_tx(&self, hash: Hash256, data: Vec<u8>) -> bool {
        let mut known = self.known_txs.lock().unwrap();
        if known.contains(&hash) {
            return false;
        }
        known.put(hash, data);

        // Queue the hash for the next outbound broadcast
        let mut queue = self.outbound_queue.lock().unwrap();
        if queue.len() < MAX_BATCH_SIZE {
            queue.push(hash);
        }
        true
    }

    /// Check if we already know a transaction by hash.
    pub fn has_tx(&self, hash: &Hash256) -> bool {
        self.known_txs.lock().unwrap().contains(hash)
    }

    /// Get transaction data by hash.
    pub fn get_tx_data(&self, hash: &Hash256) -> Option<Vec<u8>> {
        self.known_txs.lock().unwrap().get(hash).cloned()
    }

    /// Take accumulated hashes for broadcasting as TMHaveTransactions.
    /// Returns up to MAX_BATCH_SIZE hashes and clears the queue.
    pub fn drain_outbound_queue(&self) -> Vec<Hash256> {
        let mut queue = self.outbound_queue.lock().unwrap();
        let batch: Vec<Hash256> = queue.drain(..).take(MAX_BATCH_SIZE).collect();
        batch
    }

    /// Process an inbound TMHaveTransactions message.
    ///
    /// Returns the list of hashes we do not have and have not already requested,
    /// so the caller can request them from the sender.
    pub fn process_have_transactions(&self, hash_bytes: &[Vec<u8>]) -> Vec<Hash256> {
        let known = self.known_txs.lock().unwrap();
        let mut pending = self.pending_requests.lock().unwrap();
        let mut missing = Vec::new();

        for raw_hash in hash_bytes {
            if raw_hash.len() != 32 {
                continue;
            }
            let arr: [u8; 32] = match raw_hash[..32].try_into() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let hash = Hash256::new(arr);

            // Skip if we already have it or already requested it
            if known.contains(&hash) || pending.contains(&hash) {
                continue;
            }

            pending.put(hash, ());
            missing.push(hash);

            if missing.len() >= MAX_BATCH_SIZE {
                break;
            }
        }

        missing
    }

    /// Clear a pending request entry once we receive the transaction data.
    pub fn clear_pending_request(&self, hash: &Hash256) {
        self.pending_requests.lock().unwrap().pop(hash);
    }

    /// Process an inbound TMTransactions batch message.
    ///
    /// Returns a list of (hash, raw_data) for each valid, previously-unknown
    /// transaction in the batch.
    pub fn process_transactions_batch(
        &self,
        transactions: &[TmTransaction],
    ) -> Vec<(Hash256, Vec<u8>)> {
        let mut new_txs = Vec::new();

        for tx_msg in transactions.iter().take(MAX_BATCH_SIZE) {
            let raw = match tx_msg.raw_transaction {
                Some(ref data) if !data.is_empty() => data,
                _ => continue,
            };

            // Compute tx hash: SHA-512-Half(HashPrefix::TRANSACTION_ID || raw_tx)
            let prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
            let mut hash_input = prefix.to_vec();
            hash_input.extend_from_slice(raw);
            let tx_hash = rxrpl_crypto::sha512_half::sha512_half(&[&hash_input]);

            // Clear from pending requests
            self.clear_pending_request(&tx_hash);

            // Only add if we don't already know this transaction
            if self.add_known_tx(tx_hash, raw.clone()) {
                new_txs.push((tx_hash, raw.clone()));
            }
        }

        new_txs
    }
}

// --- Encoding helpers ---

/// Encode a TMHaveTransactions message from a list of hashes.
pub fn encode_have_transactions(hashes: &[Hash256]) -> Vec<u8> {
    let msg = TmHaveTransactions {
        hashes: hashes.iter().map(|h| h.as_bytes().to_vec()).collect(),
    };
    msg.encode_to_vec()
}

/// Encode a TMTransactions batch message from transaction data.
pub fn encode_transactions_batch(txs: &[(Hash256, Vec<u8>)]) -> Vec<u8> {
    let msg = TmTransactions {
        transactions: txs
            .iter()
            .map(|(_hash, data)| TmTransaction {
                raw_transaction: Some(data.clone()),
                status: Some(0),
                receive_timestamp: Some(0),
                deferred: Some(false),
            })
            .collect(),
    };
    msg.encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tx_hash(data: &[u8]) -> Hash256 {
        let prefix = rxrpl_crypto::hash_prefix::HashPrefix::TRANSACTION_ID.to_bytes();
        let mut input = prefix.to_vec();
        input.extend_from_slice(data);
        rxrpl_crypto::sha512_half::sha512_half(&[&input])
    }

    #[test]
    fn add_known_tx_returns_true_for_new() {
        let relay = TxBatchRelay::new();
        let data = vec![1, 2, 3, 4];
        let hash = make_tx_hash(&data);

        assert!(relay.add_known_tx(hash, data.clone()));
        assert!(!relay.add_known_tx(hash, data)); // duplicate
    }

    #[test]
    fn has_tx_checks_known_set() {
        let relay = TxBatchRelay::new();
        let data = vec![5, 6, 7];
        let hash = make_tx_hash(&data);

        assert!(!relay.has_tx(&hash));
        relay.add_known_tx(hash, data);
        assert!(relay.has_tx(&hash));
    }

    #[test]
    fn get_tx_data_returns_stored_data() {
        let relay = TxBatchRelay::new();
        let data = vec![10, 20, 30];
        let hash = make_tx_hash(&data);

        assert!(relay.get_tx_data(&hash).is_none());
        relay.add_known_tx(hash, data.clone());
        assert_eq!(relay.get_tx_data(&hash).unwrap(), data);
    }

    #[test]
    fn drain_outbound_queue_returns_accumulated_hashes() {
        let relay = TxBatchRelay::new();
        let d1 = vec![1];
        let d2 = vec![2];
        let h1 = make_tx_hash(&d1);
        let h2 = make_tx_hash(&d2);

        relay.add_known_tx(h1, d1);
        relay.add_known_tx(h2, d2);

        let batch = relay.drain_outbound_queue();
        assert_eq!(batch.len(), 2);
        assert!(batch.contains(&h1));
        assert!(batch.contains(&h2));

        // Queue should be empty now
        let batch2 = relay.drain_outbound_queue();
        assert!(batch2.is_empty());
    }

    #[test]
    fn process_have_transactions_identifies_missing() {
        let relay = TxBatchRelay::new();
        let known_data = vec![1, 2, 3];
        let known_hash = make_tx_hash(&known_data);
        relay.add_known_tx(known_hash, known_data);

        let unknown_data = vec![4, 5, 6];
        let unknown_hash = make_tx_hash(&unknown_data);

        let hashes = vec![
            known_hash.as_bytes().to_vec(),
            unknown_hash.as_bytes().to_vec(),
        ];

        let missing = relay.process_have_transactions(&hashes);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], unknown_hash);
    }

    #[test]
    fn process_have_transactions_deduplicates_requests() {
        let relay = TxBatchRelay::new();
        let data = vec![7, 8, 9];
        let hash = make_tx_hash(&data);

        let hashes = vec![hash.as_bytes().to_vec()];

        let first = relay.process_have_transactions(&hashes);
        assert_eq!(first.len(), 1);

        // Second time, same hash should be filtered as already requested
        let second = relay.process_have_transactions(&hashes);
        assert!(second.is_empty());
    }

    #[test]
    fn process_have_transactions_skips_invalid_hashes() {
        let relay = TxBatchRelay::new();

        let hashes = vec![
            vec![0xFF; 31], // too short
            vec![0xFF; 33], // too long (but first 32 will be used -- actually should skip)
            vec![0xAA; 32], // valid
        ];

        let missing = relay.process_have_transactions(&hashes);
        // Only the 32-byte hash should be processed
        assert_eq!(missing.len(), 1);
    }

    #[test]
    fn process_transactions_batch_adds_new_txs() {
        let relay = TxBatchRelay::new();
        let d1 = vec![10, 11, 12];
        let d2 = vec![13, 14, 15];

        let batch = vec![
            TmTransaction {
                raw_transaction: Some(d1.clone()),
                status: Some(0),
                receive_timestamp: Some(0),
                deferred: Some(false),
            },
            TmTransaction {
                raw_transaction: Some(d2.clone()),
                status: Some(0),
                receive_timestamp: Some(0),
                deferred: Some(false),
            },
        ];

        let new_txs = relay.process_transactions_batch(&batch);
        assert_eq!(new_txs.len(), 2);

        let h1 = make_tx_hash(&d1);
        let h2 = make_tx_hash(&d2);
        assert!(relay.has_tx(&h1));
        assert!(relay.has_tx(&h2));
    }

    #[test]
    fn process_transactions_batch_skips_known() {
        let relay = TxBatchRelay::new();
        let data = vec![20, 21];
        let hash = make_tx_hash(&data);
        relay.add_known_tx(hash, data.clone());

        let batch = vec![TmTransaction {
            raw_transaction: Some(data),
            status: Some(0),
            receive_timestamp: Some(0),
            deferred: Some(false),
        }];

        let new_txs = relay.process_transactions_batch(&batch);
        assert!(new_txs.is_empty());
    }

    #[test]
    fn process_transactions_batch_skips_empty() {
        let relay = TxBatchRelay::new();

        let batch = vec![
            TmTransaction {
                raw_transaction: None,
                status: Some(0),
                receive_timestamp: Some(0),
                deferred: Some(false),
            },
            TmTransaction {
                raw_transaction: Some(vec![]),
                status: Some(0),
                receive_timestamp: Some(0),
                deferred: Some(false),
            },
        ];

        let new_txs = relay.process_transactions_batch(&batch);
        assert!(new_txs.is_empty());
    }

    #[test]
    fn encode_have_transactions_roundtrip() {
        let h1 = Hash256::new([0x01; 32]);
        let h2 = Hash256::new([0x02; 32]);

        let encoded = encode_have_transactions(&[h1, h2]);
        let decoded = TmHaveTransactions::decode(encoded.as_slice()).unwrap();

        assert_eq!(decoded.hashes.len(), 2);
        assert_eq!(decoded.hashes[0], h1.as_bytes().to_vec());
        assert_eq!(decoded.hashes[1], h2.as_bytes().to_vec());
    }

    #[test]
    fn encode_transactions_batch_roundtrip() {
        let d1 = vec![1, 2, 3];
        let d2 = vec![4, 5, 6];
        let h1 = make_tx_hash(&d1);
        let h2 = make_tx_hash(&d2);

        let encoded = encode_transactions_batch(&[(h1, d1.clone()), (h2, d2.clone())]);
        let decoded = TmTransactions::decode(encoded.as_slice()).unwrap();

        assert_eq!(decoded.transactions.len(), 2);
        assert_eq!(
            decoded.transactions[0].raw_transaction.as_ref().unwrap(),
            &d1
        );
        assert_eq!(
            decoded.transactions[1].raw_transaction.as_ref().unwrap(),
            &d2
        );
    }

    #[test]
    fn batch_size_cap_on_outbound_queue() {
        let relay = TxBatchRelay::new();

        for i in 0..MAX_BATCH_SIZE + 10 {
            let data = vec![i as u8; 4];
            let hash = make_tx_hash(&data);
            relay.add_known_tx(hash, data);
        }

        // The outbound queue should be capped at MAX_BATCH_SIZE
        let queue = relay.outbound_queue.lock().unwrap();
        assert_eq!(queue.len(), MAX_BATCH_SIZE);
    }

    #[test]
    fn clear_pending_request_allows_re_request() {
        let relay = TxBatchRelay::new();
        let data = vec![30, 31, 32];
        let hash = make_tx_hash(&data);

        let hashes = vec![hash.as_bytes().to_vec()];

        // First request
        let first = relay.process_have_transactions(&hashes);
        assert_eq!(first.len(), 1);

        // Pending, so second request is empty
        let second = relay.process_have_transactions(&hashes);
        assert!(second.is_empty());

        // Clear the pending request
        relay.clear_pending_request(&hash);

        // Now it should be requestable again
        let third = relay.process_have_transactions(&hashes);
        assert_eq!(third.len(), 1);
    }
}
