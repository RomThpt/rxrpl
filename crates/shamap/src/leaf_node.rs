use std::sync::Arc;

use rxrpl_crypto::hash_prefix::HashPrefix;
use rxrpl_crypto::sha512_half::sha512_half;
use rxrpl_primitives::Hash256;

use crate::item::SHAMapItem;

/// Data stored in a leaf node: the item and its cached hash.
#[derive(Clone, Debug)]
pub struct LeafData {
    item: Arc<SHAMapItem>,
    hash: Hash256,
}

/// A leaf node in the SHAMap.
///
/// Three variants with different hash prefixes:
/// - AccountState: SHA-512/2(LEAF_NODE || data || key)
/// - TransactionNoMeta: SHA-512/2(TRANSACTION_ID || data)
/// - TransactionWithMeta: SHA-512/2(TX_NODE || data || key)
#[derive(Clone, Debug)]
pub enum LeafNode {
    AccountState(LeafData),
    TransactionNoMeta(LeafData),
    TransactionWithMeta(LeafData),
}

impl LeafNode {
    /// Create a new AccountState leaf.
    pub fn account_state(key: Hash256, data: Vec<u8>) -> Self {
        let item = Arc::new(SHAMapItem::new(key, data));
        let hash = Self::hash_account_state(&item);
        LeafNode::AccountState(LeafData { item, hash })
    }

    /// Create a new TransactionNoMeta leaf.
    pub fn transaction_no_meta(key: Hash256, data: Vec<u8>) -> Self {
        let item = Arc::new(SHAMapItem::new(key, data));
        let hash = Self::hash_tx_no_meta(&item);
        LeafNode::TransactionNoMeta(LeafData { item, hash })
    }

    /// Create a new TransactionWithMeta leaf.
    pub fn transaction_with_meta(key: Hash256, data: Vec<u8>) -> Self {
        let item = Arc::new(SHAMapItem::new(key, data));
        let hash = Self::hash_tx_with_meta(&item);
        LeafNode::TransactionWithMeta(LeafData { item, hash })
    }

    fn leaf_data(&self) -> &LeafData {
        match self {
            LeafNode::AccountState(d) => d,
            LeafNode::TransactionNoMeta(d) => d,
            LeafNode::TransactionWithMeta(d) => d,
        }
    }

    fn leaf_data_mut(&mut self) -> &mut LeafData {
        match self {
            LeafNode::AccountState(d) => d,
            LeafNode::TransactionNoMeta(d) => d,
            LeafNode::TransactionWithMeta(d) => d,
        }
    }

    pub fn hash(&self) -> Hash256 {
        self.leaf_data().hash
    }

    pub fn key(&self) -> &Hash256 {
        self.leaf_data().item.key()
    }

    pub fn data(&self) -> &[u8] {
        self.leaf_data().item.data()
    }

    pub fn item(&self) -> &Arc<SHAMapItem> {
        &self.leaf_data().item
    }

    /// Replace the item and recompute the hash.
    pub fn update_data(&mut self, data: Vec<u8>) {
        let key = *self.key();
        let item = Arc::new(SHAMapItem::new(key, data));
        let hash = match self {
            LeafNode::AccountState(_) => Self::hash_account_state(&item),
            LeafNode::TransactionNoMeta(_) => Self::hash_tx_no_meta(&item),
            LeafNode::TransactionWithMeta(_) => Self::hash_tx_with_meta(&item),
        };
        let ld = self.leaf_data_mut();
        ld.item = item;
        ld.hash = hash;
    }

    /// Create a leaf of the same variant with new key and data.
    pub fn new_same_type(&self, key: Hash256, data: Vec<u8>) -> Self {
        match self {
            LeafNode::AccountState(_) => Self::account_state(key, data),
            LeafNode::TransactionNoMeta(_) => Self::transaction_no_meta(key, data),
            LeafNode::TransactionWithMeta(_) => Self::transaction_with_meta(key, data),
        }
    }

    fn hash_account_state(item: &SHAMapItem) -> Hash256 {
        let prefix = HashPrefix::LEAF_NODE.to_bytes();
        sha512_half(&[&prefix, item.data(), item.key().as_bytes()])
    }

    fn hash_tx_no_meta(item: &SHAMapItem) -> Hash256 {
        let prefix = HashPrefix::TRANSACTION_ID.to_bytes();
        sha512_half(&[&prefix, item.data()])
    }

    fn hash_tx_with_meta(item: &SHAMapItem) -> Hash256 {
        let prefix = HashPrefix::TX_NODE.to_bytes();
        sha512_half(&[&prefix, item.data(), item.key().as_bytes()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_state_hash() {
        let key = Hash256::ZERO;
        let data = vec![1, 2, 3, 4];
        let leaf = LeafNode::account_state(key, data.clone());
        assert!(!leaf.hash().is_zero());

        let leaf2 = LeafNode::account_state(key, data);
        assert_eq!(leaf.hash(), leaf2.hash());
    }

    #[test]
    fn different_types_different_hashes() {
        let key = Hash256::ZERO;
        let data = vec![1, 2, 3, 4];
        let h1 = LeafNode::account_state(key, data.clone()).hash();
        let h2 = LeafNode::transaction_no_meta(key, data.clone()).hash();
        let h3 = LeafNode::transaction_with_meta(key, data).hash();
        assert_ne!(h1, h2);
        assert_ne!(h1, h3);
        assert_ne!(h2, h3);
    }

    #[test]
    fn update_data_changes_hash() {
        let key = Hash256::ZERO;
        let mut leaf = LeafNode::account_state(key, vec![1, 2, 3]);
        let old_hash = leaf.hash();
        leaf.update_data(vec![4, 5, 6]);
        assert_ne!(leaf.hash(), old_hash);
        assert_eq!(leaf.data(), &[4, 5, 6]);
    }

    #[test]
    fn key_and_data_accessors() {
        let key = Hash256::new([0xAB; 32]);
        let data = vec![10, 20, 30];
        let leaf = LeafNode::transaction_no_meta(key, data.clone());
        assert_eq!(*leaf.key(), key);
        assert_eq!(leaf.data(), &data[..]);
    }

    #[test]
    fn new_same_type() {
        let key1 = Hash256::new([0x01; 32]);
        let key2 = Hash256::new([0x02; 32]);
        let leaf = LeafNode::transaction_with_meta(key1, vec![1]);
        let leaf2 = leaf.new_same_type(key2, vec![2]);
        assert!(matches!(leaf2, LeafNode::TransactionWithMeta(_)));
        assert_eq!(*leaf2.key(), key2);
        assert_eq!(leaf2.data(), &[2]);
    }
}
