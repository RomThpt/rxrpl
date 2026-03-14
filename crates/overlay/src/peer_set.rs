use std::sync::Arc;

use dashmap::DashMap;
use rxrpl_primitives::Hash256;

/// Thread-safe collection of connected peers.
pub struct PeerSet {
    peers: DashMap<Hash256, Arc<PeerInfo>>,
    max_peers: usize,
}

/// Information about a connected peer.
#[derive(Debug)]
pub struct PeerInfo {
    /// The peer's node ID.
    pub node_id: Hash256,
    /// Remote address.
    pub address: String,
    /// Whether this is an inbound or outbound connection.
    pub inbound: bool,
    /// Last known ledger sequence from this peer.
    pub ledger_seq: std::sync::atomic::AtomicU32,
}

impl PeerSet {
    pub fn new(max_peers: usize) -> Self {
        Self {
            peers: DashMap::new(),
            max_peers,
        }
    }

    /// Add a peer. Returns false if the peer limit is reached.
    pub fn add(&self, info: Arc<PeerInfo>) -> bool {
        if self.peers.len() >= self.max_peers {
            return false;
        }
        self.peers.insert(info.node_id, info);
        true
    }

    /// Remove a peer by node ID.
    pub fn remove(&self, node_id: &Hash256) -> Option<Arc<PeerInfo>> {
        self.peers.remove(node_id).map(|(_, v)| v)
    }

    /// Get a peer by node ID.
    pub fn get(&self, node_id: &Hash256) -> Option<Arc<PeerInfo>> {
        self.peers.get(node_id).map(|r| Arc::clone(r.value()))
    }

    /// Number of connected peers.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Get all peer node IDs.
    pub fn peer_ids(&self) -> Vec<Hash256> {
        self.peers.iter().map(|r| *r.key()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peer(id_byte: u8, inbound: bool) -> Arc<PeerInfo> {
        Arc::new(PeerInfo {
            node_id: Hash256::new([id_byte; 32]),
            address: format!("127.0.0.1:{}", 51235 + id_byte as u16),
            inbound,
            ledger_seq: std::sync::atomic::AtomicU32::new(0),
        })
    }

    #[test]
    fn add_and_get() {
        let set = PeerSet::new(10);
        let peer = make_peer(1, false);
        assert!(set.add(peer.clone()));
        assert_eq!(set.len(), 1);
        assert!(set.get(&Hash256::new([1; 32])).is_some());
    }

    #[test]
    fn peer_limit() {
        let set = PeerSet::new(1);
        assert!(set.add(make_peer(1, false)));
        assert!(!set.add(make_peer(2, false)));
    }

    #[test]
    fn remove_peer() {
        let set = PeerSet::new(10);
        let id = Hash256::new([1; 32]);
        set.add(make_peer(1, false));
        assert!(set.remove(&id).is_some());
        assert!(set.is_empty());
    }
}
