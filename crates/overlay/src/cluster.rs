use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Information tracked for each cluster peer node.
#[derive(Debug, Clone)]
pub struct ClusterNode {
    /// The node's public key (hex-encoded).
    pub public_key: String,
    /// Current load fee reported by the node.
    pub load_fee: u32,
    /// Human-readable name of the node.
    pub name: String,
    /// Network address reported by the node.
    pub address: String,
    /// When we last received a report from this node.
    pub last_report: Instant,
    /// The report_time value from the last TMCluster message.
    pub report_time: u32,
}

/// Manages cluster membership and status tracking.
///
/// In rippled, cluster nodes share load-balancing and fee information
/// through TMCluster (type 5) messages. Cluster peers are trusted for
/// fee adjustments and receive priority in transaction relay.
pub struct ClusterManager {
    /// Whether cluster mode is active.
    enabled: bool,
    /// Our own human-readable node name.
    node_name: String,
    /// Set of configured cluster member public keys.
    configured_members: HashMap<String, ()>,
    /// Active cluster node state, keyed by public key.
    active_nodes: HashMap<String, ClusterNode>,
    /// Maximum age for a cluster node report before it is considered stale.
    stale_threshold: Duration,
}

impl ClusterManager {
    /// Create a new cluster manager from configuration.
    pub fn new(enabled: bool, node_name: String, member_keys: Vec<String>) -> Self {
        let configured_members: HashMap<String, ()> =
            member_keys.into_iter().map(|k| (k, ())).collect();
        Self {
            enabled,
            node_name,
            configured_members,
            active_nodes: HashMap::new(),
            stale_threshold: Duration::from_secs(120),
        }
    }

    /// Check whether cluster mode is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Check whether a public key belongs to a configured cluster member.
    pub fn is_member(&self, public_key: &str) -> bool {
        self.configured_members.contains_key(public_key)
    }

    /// Return the number of configured cluster members.
    pub fn member_count(&self) -> usize {
        self.configured_members.len()
    }

    /// Return our node name for cluster broadcasts.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Update a cluster node's status from a received TMCluster message.
    ///
    /// Returns `true` if the sender is a known cluster member and the update
    /// was applied, `false` if the sender is not a cluster member.
    pub fn update_node(
        &mut self,
        public_key: &str,
        load_fee: u32,
        name: &str,
        address: &str,
        report_time: u32,
    ) -> bool {
        if !self.is_member(public_key) {
            return false;
        }

        let node = self
            .active_nodes
            .entry(public_key.to_string())
            .or_insert_with(|| ClusterNode {
                public_key: public_key.to_string(),
                load_fee: 0,
                name: String::new(),
                address: String::new(),
                last_report: Instant::now(),
                report_time: 0,
            });

        node.load_fee = load_fee;
        if !name.is_empty() {
            node.name = name.to_string();
        }
        if !address.is_empty() {
            node.address = address.to_string();
        }
        node.report_time = report_time;
        node.last_report = Instant::now();
        true
    }

    /// Get all active (non-stale) cluster nodes.
    pub fn active_nodes(&self) -> Vec<&ClusterNode> {
        let now = Instant::now();
        self.active_nodes
            .values()
            .filter(|n| now.duration_since(n.last_report) < self.stale_threshold)
            .collect()
    }

    /// Get a specific cluster node by public key.
    pub fn get_node(&self, public_key: &str) -> Option<&ClusterNode> {
        self.active_nodes.get(public_key)
    }

    /// Remove stale cluster nodes (no report within the threshold).
    pub fn prune_stale(&mut self) -> usize {
        let now = Instant::now();
        let threshold = self.stale_threshold;
        let before = self.active_nodes.len();
        self.active_nodes
            .retain(|_, n| now.duration_since(n.last_report) < threshold);
        before - self.active_nodes.len()
    }

    /// Compute the cluster-wide average load fee from active nodes.
    ///
    /// Returns `None` if no active cluster nodes have reported.
    pub fn average_load_fee(&self) -> Option<u32> {
        let active = self.active_nodes();
        if active.is_empty() {
            return None;
        }
        let sum: u64 = active.iter().map(|n| n.load_fee as u64).sum();
        Some((sum / active.len() as u64) as u32)
    }

    /// Get a serializable summary of cluster state for RPC responses.
    pub fn cluster_info(&self) -> Vec<ClusterNodeInfo> {
        let now = Instant::now();
        self.active_nodes
            .values()
            .map(|n| ClusterNodeInfo {
                public_key: n.public_key.clone(),
                name: n.name.clone(),
                load_fee: n.load_fee,
                address: n.address.clone(),
                age_secs: now.duration_since(n.last_report).as_secs(),
            })
            .collect()
    }

    /// Get all configured member keys.
    pub fn configured_member_keys(&self) -> Vec<&String> {
        self.configured_members.keys().collect()
    }
}

/// Serializable cluster node information for RPC responses.
#[derive(Debug, Clone)]
pub struct ClusterNodeInfo {
    pub public_key: String,
    pub name: String,
    pub load_fee: u32,
    pub address: String,
    pub age_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager(keys: Vec<&str>) -> ClusterManager {
        ClusterManager::new(
            true,
            "test-node".to_string(),
            keys.into_iter().map(String::from).collect(),
        )
    }

    #[test]
    fn membership_check() {
        let mgr = make_manager(vec!["key_a", "key_b"]);
        assert!(mgr.is_enabled());
        assert!(mgr.is_member("key_a"));
        assert!(mgr.is_member("key_b"));
        assert!(!mgr.is_member("key_c"));
        assert_eq!(mgr.member_count(), 2);
    }

    #[test]
    fn update_known_member() {
        let mut mgr = make_manager(vec!["key_a"]);
        assert!(mgr.update_node("key_a", 256, "node-a", "10.0.0.1:51235", 1000));

        let node = mgr.get_node("key_a").unwrap();
        assert_eq!(node.load_fee, 256);
        assert_eq!(node.name, "node-a");
        assert_eq!(node.address, "10.0.0.1:51235");
        assert_eq!(node.report_time, 1000);
    }

    #[test]
    fn reject_unknown_member() {
        let mut mgr = make_manager(vec!["key_a"]);
        assert!(!mgr.update_node("unknown_key", 100, "rogue", "", 0));
        assert!(mgr.get_node("unknown_key").is_none());
    }

    #[test]
    fn active_nodes_returns_only_fresh() {
        let mut mgr = make_manager(vec!["key_a", "key_b"]);
        mgr.update_node("key_a", 100, "a", "", 1);
        mgr.update_node("key_b", 200, "b", "", 2);
        assert_eq!(mgr.active_nodes().len(), 2);
    }

    #[test]
    fn average_load_fee_calculation() {
        let mut mgr = make_manager(vec!["a", "b", "c"]);
        mgr.update_node("a", 100, "", "", 0);
        mgr.update_node("b", 200, "", "", 0);
        mgr.update_node("c", 300, "", "", 0);
        assert_eq!(mgr.average_load_fee(), Some(200));
    }

    #[test]
    fn average_load_fee_empty() {
        let mgr = make_manager(vec!["a"]);
        assert_eq!(mgr.average_load_fee(), None);
    }

    #[test]
    fn cluster_info_snapshot() {
        let mut mgr = make_manager(vec!["key_x"]);
        mgr.update_node("key_x", 500, "node-x", "1.2.3.4:51235", 42);
        let info = mgr.cluster_info();
        assert_eq!(info.len(), 1);
        assert_eq!(info[0].public_key, "key_x");
        assert_eq!(info[0].load_fee, 500);
        assert_eq!(info[0].name, "node-x");
    }

    #[test]
    fn disabled_cluster() {
        let mgr = ClusterManager::new(false, String::new(), Vec::new());
        assert!(!mgr.is_enabled());
        assert_eq!(mgr.member_count(), 0);
    }

    #[test]
    fn prune_stale_removes_nothing_when_fresh() {
        let mut mgr = make_manager(vec!["a"]);
        mgr.update_node("a", 100, "a", "", 0);
        assert_eq!(mgr.prune_stale(), 0);
        assert_eq!(mgr.active_nodes().len(), 1);
    }

    #[test]
    fn update_overwrites_previous_values() {
        let mut mgr = make_manager(vec!["key_a"]);
        mgr.update_node("key_a", 100, "name1", "addr1", 1);
        mgr.update_node("key_a", 200, "name2", "addr2", 2);

        let node = mgr.get_node("key_a").unwrap();
        assert_eq!(node.load_fee, 200);
        assert_eq!(node.name, "name2");
        assert_eq!(node.address, "addr2");
        assert_eq!(node.report_time, 2);
    }

    #[test]
    fn configured_member_keys_returns_all() {
        let mgr = make_manager(vec!["x", "y", "z"]);
        let mut keys: Vec<&str> = mgr
            .configured_member_keys()
            .iter()
            .map(|k| k.as_str())
            .collect();
        keys.sort();
        assert_eq!(keys, vec!["x", "y", "z"]);
    }
}
