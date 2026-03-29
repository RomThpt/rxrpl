use std::collections::{HashMap, HashSet};

use rxrpl_primitives::PublicKey;

use crate::types::NodeId;

/// Trusted Validator List (UNL).
///
/// Determines which validators count toward consensus quorum.
/// An empty UNL means solo mode (all proposals accepted).
///
/// Also tracks the ephemeral -> master key mapping so that
/// validations signed by ephemeral keys can be attributed
/// to the correct trusted validator.
#[derive(Clone, Debug, Default)]
pub struct TrustedValidatorList {
    /// Validators we trust (by NodeId derived from master key).
    trusted: HashSet<NodeId>,
    /// Validators temporarily removed from quorum (e.g., unreliable).
    negative_unl: HashSet<NodeId>,
    /// Ephemeral NodeId -> master NodeId mapping.
    /// Populated from verified manifests so that validations signed
    /// by an ephemeral key can be resolved to a trusted master key.
    ephemeral_to_master: HashMap<NodeId, NodeId>,
    /// Master public key bytes (hex) -> NodeId, for reverse lookups.
    master_keys: HashMap<String, NodeId>,
}

impl TrustedValidatorList {
    /// Create a new UNL from a set of trusted node IDs.
    pub fn new(trusted: HashSet<NodeId>) -> Self {
        Self {
            trusted,
            negative_unl: HashSet::new(),
            ephemeral_to_master: HashMap::new(),
            master_keys: HashMap::new(),
        }
    }

    /// Create an empty UNL (solo mode).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if a node is trusted (and not in the negative UNL).
    ///
    /// Accepts both master NodeIds and ephemeral NodeIds (resolved
    /// through the manifest mapping).
    pub fn is_trusted(&self, node_id: &NodeId) -> bool {
        let resolved = self.resolve_to_master(node_id);
        self.trusted.contains(resolved) && !self.negative_unl.contains(resolved)
    }

    /// Resolve an ephemeral NodeId to its master NodeId.
    /// Returns the input unchanged if no mapping exists.
    pub fn resolve_to_master<'a>(&'a self, node_id: &'a NodeId) -> &'a NodeId {
        self.ephemeral_to_master.get(node_id).unwrap_or(node_id)
    }

    /// Check if the UNL is empty (solo mode).
    pub fn is_empty(&self) -> bool {
        self.trusted.is_empty()
    }

    /// Add a node to the negative UNL.
    pub fn add_to_negative_unl(&mut self, node_id: NodeId) {
        self.negative_unl.insert(node_id);
    }

    /// Remove a node from the negative UNL.
    pub fn remove_from_negative_unl(&mut self, node_id: &NodeId) {
        self.negative_unl.remove(node_id);
    }

    /// Effective size: trusted minus negative UNL.
    pub fn effective_size(&self) -> usize {
        self.trusted
            .iter()
            .filter(|n| !self.negative_unl.contains(n))
            .count()
    }

    /// Quorum threshold: 80% of effective size, rounded up.
    pub fn quorum_threshold(&self) -> usize {
        let size = self.effective_size();
        if size == 0 {
            return 0;
        }
        (size * 80).div_ceil(100)
    }

    /// Replace the trusted set with validators from a verified validator list.
    ///
    /// Each public key is hashed to produce a NodeId.
    pub fn update_from_validator_keys(&mut self, master_keys: &[PublicKey]) {
        self.trusted.clear();
        self.master_keys.clear();
        for pk in master_keys {
            let node_id = NodeId::from_public_key(pk.as_bytes());
            self.trusted.insert(node_id);
            self.master_keys
                .insert(hex::encode(pk.as_bytes()), node_id);
        }
    }

    /// Register an ephemeral key mapping from a verified manifest.
    ///
    /// If `old_ephemeral` is provided, its mapping is removed first.
    pub fn register_ephemeral_key(
        &mut self,
        master_pk: &PublicKey,
        ephemeral_pk: &PublicKey,
        old_ephemeral: Option<&PublicKey>,
    ) {
        let master_id = NodeId::from_public_key(master_pk.as_bytes());
        let eph_id = NodeId::from_public_key(ephemeral_pk.as_bytes());

        // Remove old mapping
        if let Some(old) = old_ephemeral {
            let old_id = NodeId::from_public_key(old.as_bytes());
            self.ephemeral_to_master.remove(&old_id);
        }

        self.ephemeral_to_master.insert(eph_id, master_id);
    }

    /// Remove an ephemeral key mapping (e.g., on revocation).
    pub fn remove_ephemeral_key(&mut self, ephemeral_pk: &PublicKey) {
        let eph_id = NodeId::from_public_key(ephemeral_pk.as_bytes());
        self.ephemeral_to_master.remove(&eph_id);
    }

    /// Remove a master key from the trusted set (e.g., on revocation).
    pub fn revoke_master_key(&mut self, master_pk: &PublicKey) {
        let node_id = NodeId::from_public_key(master_pk.as_bytes());
        self.trusted.remove(&node_id);
        let hex_key = hex::encode(master_pk.as_bytes());
        self.master_keys.remove(&hex_key);
    }

    /// Get a reference to the trusted set.
    pub fn trusted_set(&self) -> &HashSet<NodeId> {
        &self.trusted
    }

    /// Get a reference to the negative UNL set.
    pub fn negative_unl_set(&self) -> &HashSet<NodeId> {
        &self.negative_unl
    }

    /// Check if a node is in the negative UNL.
    pub fn is_in_negative_unl(&self, node_id: &NodeId) -> bool {
        self.negative_unl.contains(node_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rxrpl_primitives::Hash256;

    fn node(id: u8) -> NodeId {
        NodeId(Hash256::new([id; 32]))
    }

    #[test]
    fn empty_unl_is_solo() {
        let unl = TrustedValidatorList::empty();
        assert!(unl.is_empty());
        assert_eq!(unl.effective_size(), 0);
        assert_eq!(unl.quorum_threshold(), 0);
    }

    #[test]
    fn trusted_check() {
        let mut trusted = HashSet::new();
        trusted.insert(node(1));
        trusted.insert(node(2));
        let unl = TrustedValidatorList::new(trusted);

        assert!(unl.is_trusted(&node(1)));
        assert!(unl.is_trusted(&node(2)));
        assert!(!unl.is_trusted(&node(3)));
    }

    #[test]
    fn negative_unl_reduces_effective_size() {
        let mut trusted = HashSet::new();
        for i in 1..=5 {
            trusted.insert(node(i));
        }
        let mut unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.effective_size(), 5);
        assert_eq!(unl.quorum_threshold(), 4); // ceil(5*0.8) = 4

        unl.add_to_negative_unl(node(1));
        assert_eq!(unl.effective_size(), 4);
        assert!(!unl.is_trusted(&node(1)));

        unl.remove_from_negative_unl(&node(1));
        assert_eq!(unl.effective_size(), 5);
        assert!(unl.is_trusted(&node(1)));
    }

    #[test]
    fn quorum_threshold_rounding() {
        // 10 validators -> 80% = 8
        let mut trusted = HashSet::new();
        for i in 1..=10 {
            trusted.insert(node(i));
        }
        let unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.quorum_threshold(), 8);

        // 3 validators -> ceil(3*0.8) = ceil(2.4) = 3
        let mut trusted = HashSet::new();
        for i in 1..=3 {
            trusted.insert(node(i));
        }
        let unl = TrustedValidatorList::new(trusted);
        assert_eq!(unl.quorum_threshold(), 3);
    }

    #[test]
    fn update_from_validator_keys() {
        let mut unl = TrustedValidatorList::empty();
        assert!(unl.is_empty());

        let kp1 = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("val1"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let kp2 = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("val2"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        unl.update_from_validator_keys(&[kp1.public_key.clone(), kp2.public_key.clone()]);
        assert_eq!(unl.effective_size(), 2);

        let id1 = NodeId::from_public_key(kp1.public_key.as_bytes());
        assert!(unl.is_trusted(&id1));
    }

    #[test]
    fn ephemeral_key_resolution() {
        let mut unl = TrustedValidatorList::empty();

        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("master_eph_test"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("ephemeral_eph_test"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        // Add master to trusted
        unl.update_from_validator_keys(&[master_kp.public_key.clone()]);

        // Register ephemeral mapping
        unl.register_ephemeral_key(&master_kp.public_key, &eph_kp.public_key, None);

        // Ephemeral ID should resolve to trusted
        let eph_id = NodeId::from_public_key(eph_kp.public_key.as_bytes());
        assert!(unl.is_trusted(&eph_id));

        // Resolve explicitly
        let master_id = NodeId::from_public_key(master_kp.public_key.as_bytes());
        assert_eq!(unl.resolve_to_master(&eph_id), &master_id);
    }

    #[test]
    fn revoke_master_key() {
        let mut unl = TrustedValidatorList::empty();

        let kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("revoke_test"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        unl.update_from_validator_keys(&[kp.public_key.clone()]);
        let id = NodeId::from_public_key(kp.public_key.as_bytes());
        assert!(unl.is_trusted(&id));

        unl.revoke_master_key(&kp.public_key);
        assert!(!unl.is_trusted(&id));
    }

    #[test]
    fn ephemeral_key_rotation() {
        let mut unl = TrustedValidatorList::empty();

        let master_kp = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("master_rot"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph1 = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("eph_rot_1"),
            rxrpl_crypto::KeyType::Ed25519,
        );
        let eph2 = rxrpl_crypto::KeyPair::from_seed(
            &rxrpl_crypto::Seed::from_passphrase("eph_rot_2"),
            rxrpl_crypto::KeyType::Ed25519,
        );

        unl.update_from_validator_keys(&[master_kp.public_key.clone()]);

        // Register first ephemeral
        unl.register_ephemeral_key(&master_kp.public_key, &eph1.public_key, None);
        let eph1_id = NodeId::from_public_key(eph1.public_key.as_bytes());
        assert!(unl.is_trusted(&eph1_id));

        // Rotate to second ephemeral
        unl.register_ephemeral_key(
            &master_kp.public_key,
            &eph2.public_key,
            Some(&eph1.public_key),
        );

        // Old ephemeral no longer resolves
        assert!(!unl.is_trusted(&eph1_id));
        // New ephemeral resolves
        let eph2_id = NodeId::from_public_key(eph2.public_key.as_bytes());
        assert!(unl.is_trusted(&eph2_id));
    }
}
